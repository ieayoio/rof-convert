# rof-convert 生产镜像
#
# 构建（默认使用国内 Cargo 镜像 rsproxy 和中科大 Debian 镜像）：
#   docker build -t rof-convert:0.1.0 .
#
# LibreOffice 不取自发行版仓库，而是构建时从 The Document Foundation 官方站点
# 下载官方 .deb 包，默认安装最新稳定版。Debian bookworm 自带的 7.4 缺少新过滤器
# （例如 Markdown/CommonMark 需要 26.2+），官方包才能让镜像跟上最新功能。
# 官方包没有发行版那样的 -nogui 精简变体，镜像会比发行版方案大约数百 MB，因此
# 下面会删掉帮助、图库、模板等运行期用不到的内容。
#
# 常用构建参数：
#   --build-arg LO_VERSION=26.8.1                               固定版本，构建可复现
#   --build-arg LO_MIRROR=https://mirrors.aliyun.com/tdf         改用国内镜像
#   --build-arg RUST_IMAGE=<mirror>/library/rust:1.99-bookworm  基础镜像代理
#   --build-arg DEBIAN_IMAGE=<mirror>/library/debian:bookworm-slim
#   --build-arg CARGO_REGISTRY=...                              切换 Cargo 镜像
#   --build-arg DEBIAN_MIRROR=...                               切换 Debian 软件源

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

# Debian 软件源默认走中科大镜像；如需其他镜像可覆盖。
# 使用 HTTP 源是为了不依赖基础镜像预装 ca-certificates，apt 仍会校验 GPG 签名。
ARG DEBIAN_MIRROR=http://mirrors.ustc.edu.cn

# LibreOffice 官方包来源。LO_MIRROR 只写到镜像的 LibreOffice 根路径，其下统一为
# /libreoffice/stable/...：官方站点是 download.documentfoundation.org，阿里云镜像
# 是 mirrors.aliyun.com/tdf（可用国内镜像加速）。
# LO_VERSION=latest 时在构建阶段解析镜像站上的最新稳定版，因此同一 commit 在不同
# 时间构建可能得到不同版本；需要可复现镜像时固定为具体版本号。
# 注意：解析 latest 依赖镜像站提供目录索引，官方站点与阿里云均支持。
ARG LO_MIRROR=https://download.documentfoundation.org
ARG LO_VERSION=latest

# BuildKit 自动注入（amd64/arm64）；官方包目录与文件名使用 x86_64 / x86-64 两套写法。
ARG TARGETARCH

# 镜像默认按生产环境启动：ROF_ENV=prod 读取 config/prod.toml。
# ROF_DOCS_ENABLED=false 显式关闭 Swagger UI 与 /api/v1/openapi.json，
# 即使运行时被改成 dev 配置也不会意外暴露文档；排查时可用 -e ROF_DOCS_ENABLED=true 临时开启。
ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    HOME=/var/lib/rof-convert \
    ROF_ENV=prod \
    ROF_DOCS_ENABLED=false

# 依赖说明：
# - 官方包没有 -nogui 变体，无法像发行版那样只装 headless 子集，只能装完整套件
#   再删除非运行必需资源。
# - --no-install-recommends 必须保留：官方包的 recommends 会拉入 default-jre、
#   gstreamer 插件组等数百 MB 非必要依赖。
# - 但官方包并未把全部运行库写进 Depends，无头模式下 oosplash / soffice.bin 仍然
#   链接这些库，缺一个就启动失败（例如 libXinerama.so.1、libX11-xcb.so.1:
#   cannot open shared object file）。因此下面显式列出运行库，不依赖 apt 对本地
#   .deb 的依赖解析；列表以 soffice --version 能启动为准，升级 LO 后可能需要补充。
#   gtk3/gtk4、qt5/qt6、kf5、gstreamer 等 GUI 与 AVMedia 插件缺少的依赖不在此列，
#   无头转换不会加载它们。
# - 官方包不带 systemd/desktop 集成，install 完成后显式写一个 /usr/bin/soffice 包装
#   脚本：配置里的 soffice_bin 默认按 PATH 查找 soffice，包装脚本用 exec 保持 PID，
#   不影响超时后按进程号终止。
# - fonts-noto-cjk 是中文渲染必需项（约 91 MB），如需瘦身可换 fonts-wqy-zenhei。
# - 最新稳定版包含 Markdown（CommonMark）过滤器，服务启动探测通过后 /api/v1/formats
#   会包含 md。
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
        fontconfig \
        fonts-noto-cjk \
        libcairo2 \
        libcups2 \
        libdbus-1-3 \
        libexpat1 \
        libfontconfig1 \
        libfreetype6 \
        libglib2.0-0 \
        libice6 \
        libjpeg62-turbo \
        libltdl7 \
        libnspr4 \
        libnss3 \
        libpng16-16 \
        libsm6 \
        libx11-6 \
        libx11-xcb1 \
        libxext6 \
        libxfixes3 \
        libxinerama1 \
        libxrandr2 \
        libxrender1 \
        libxml2 \
        libxslt1.1; \
    lo_version="${LO_VERSION}"; \
    if [ "$lo_version" = "latest" ]; then \
        lo_version="$(curl -fsSL --retry 3 "${LO_MIRROR}/libreoffice/stable/" \
            | grep -o 'href="[0-9][0-9.]*/"' \
            | sed -e 's/^href="//' -e 's|/"$||' \
            | sort -V | tail -n 1)"; \
    fi; \
    if [ -z "$lo_version" ]; then \
        echo "failed to resolve LibreOffice version from ${LO_MIRROR}" >&2; \
        exit 1; \
    fi; \
    case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
        amd64) lo_dir=x86_64; lo_arch=x86-64 ;; \
        arm64) lo_dir=aarch64; lo_arch=aarch64 ;; \
        *) echo "unsupported architecture for LibreOffice packages" >&2; exit 1 ;; \
    esac; \
    lo_url="${LO_MIRROR}/libreoffice/stable/${lo_version}/deb/${lo_dir}/LibreOffice_${lo_version}_Linux_${lo_arch}_deb.tar.gz"; \
    echo "installing LibreOffice ${lo_version} from ${lo_url}"; \
    mkdir -p /tmp/libreoffice; \
    curl -fsSL --retry 3 --retry-delay 2 -o /tmp/libreoffice/lo.tar.gz "$lo_url"; \
    tar -xzf /tmp/libreoffice/lo.tar.gz -C /tmp/libreoffice; \
    lo_debs_dir=""; \
    for candidate in /tmp/libreoffice/*/DEBS /tmp/libreoffice/DEBS; do \
        if [ -d "$candidate" ]; then \
            lo_debs_dir="$candidate"; \
            break; \
        fi; \
    done; \
    if [ -z "$lo_debs_dir" ]; then \
        echo "LibreOffice DEBS directory not found in archive" >&2; \
        exit 1; \
    fi; \
    apt-get install -y --no-install-recommends "$lo_debs_dir"/*.deb; \
    lo_opt_dir="$(ls -d /opt/libreoffice*/ | head -n 1)"; \
    printf '#!/bin/sh\nexec "%sprogram/soffice" "$@"\n' "$lo_opt_dir" > /usr/bin/soffice; \
    chmod 0755 /usr/bin/soffice; \
    if ! HOME=/tmp soffice --headless --norestore --nodefault --nofirststartwizard \
        -env:UserInstallation=file:///tmp/lo-verify \
        --version; then \
        echo "LibreOffice failed to start, missing shared libraries:" >&2; \
        ldd "${lo_opt_dir}program/oosplash" "${lo_opt_dir}program/soffice.bin" \
            | awk '/not found/ { print "  " $1 }' >&2 || true; \
        exit 1; \
    fi; \
    apt-get clean; \
    rm -rf /tmp/lo-verify \
        /tmp/libreoffice \
        /var/lib/apt/lists/* \
        /usr/share/doc/* \
        /usr/share/man/* \
        /usr/share/bug/* \
        "${lo_opt_dir}readmes" \
        "${lo_opt_dir}help" \
        "${lo_opt_dir}share/gallery" \
        "${lo_opt_dir}share/template" \
        "${lo_opt_dir}share/autotext" \
        "${lo_opt_dir}share/Scripts"

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
EXPOSE 7654
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:7654/healthz > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/rof-convert"]
