# rof-convert 生产镜像
#
# 构建（默认使用国内 Cargo 镜像 rsproxy 和清华 Debian 镜像）：
#   docker build -t rof-convert:0.1.0 .
#
# 如果 Docker Hub 基础镜像拉取较慢，可通过 --build-arg 指向镜像代理：
#   docker build \
#     --build-arg RUST_IMAGE=<mirror>/library/rust:1.99-bookworm \
#     --build-arg DEBIAN_IMAGE=<mirror>/library/debian:bookworm-slim \
#     -t rof-convert:0.1.0 .
#
# 需要切换 Cargo/Debian 镜像时也可覆盖对应 build-arg。

ARG RUST_IMAGE=rust:1.99-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-slim

FROM ${RUST_IMAGE} AS builder

# 默认使用 rsproxy；可改为 USTC/TUNA 等国内 Cargo 镜像。
# 示例：
#   --build-arg CARGO_REGISTRY=sparse+https://mirrors.ustc.edu.cn/crates.io-index/
ARG CARGO_REGISTRY=sparse+https://rsproxy.cn/index/

ENV CARGO_HOME=/usr/local/cargo \
    CARGO_HTTP_TIMEOUT=120 \
    CARGO_NET_RETRY=5

RUN mkdir -p "$CARGO_HOME" \
    && printf '%s\n' \
        '[source.crates-io]' \
        'replace-with = "mirror"' \
        '' \
        '[source.mirror]' \
        "registry = \"${CARGO_REGISTRY}\"" \
        '' \
        '[net]' \
        'git-fetch-with-cli = true' \
        > "$CARGO_HOME/config.toml"

WORKDIR /build
COPY Cargo.toml Cargo.lock ./

# 先构建依赖层，利用 Docker 缓存减少后续源码变更时的下载/编译时间。
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked

COPY docs ./docs
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

FROM ${DEBIAN_IMAGE}

# Debian 软件源默认走清华镜像；如需其他镜像可覆盖。
# 使用 HTTP 源是为了不依赖基础镜像预装 ca-certificates，apt 仍会校验 GPG 签名。
ARG DEBIAN_MIRROR=http://mirrors.tuna.tsinghua.edu.cn

ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/var/lib/rof-convert \
    ROF_ENV=prod

RUN set -eux; \
    for file in /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources; do \
        if [ -f "$file" ]; then \
            sed -i \
                -e "s|http://deb.debian.org|${DEBIAN_MIRROR}|g" \
                -e "s|https://deb.debian.org|${DEBIAN_MIRROR}|g" \
                -e "s|http://security.debian.org|${DEBIAN_MIRROR}|g" \
                -e "s|https://security.debian.org|${DEBIAN_MIRROR}|g" \
                "$file"; \
        fi; \
    done; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        tini \
        libreoffice-writer \
        libreoffice-calc \
        libreoffice-impress \
        fontconfig \
        fonts-noto-cjk; \
    rm -rf /var/lib/apt/lists/*

RUN useradd --system --uid 10001 --user-group \
        --home-dir /var/lib/rof-convert \
        --shell /usr/sbin/nologin \
        rof-convert \
    && install -d -m 0700 -o rof-convert -g rof-convert \
        /var/lib/rof-convert \
        /var/lib/rof-convert/tmp

COPY --from=builder /build/target/release/rof-convert /usr/local/bin/rof-convert

WORKDIR /opt/rof-convert
COPY LICENSE ./LICENSE
COPY config ./config

USER 10001
EXPOSE 8080
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/rof-convert"]
