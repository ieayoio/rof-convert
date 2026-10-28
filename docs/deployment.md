# 部署与运维

本文给出 rof-convert 的生产部署建议，包括运行用户、systemd、反向代理、容器、探针、容量规划、安全加固和故障排查。接口细节见 [README](../README.md) 与 [OpenAPI 契约](openapi.json)。

## 部署前检查

1. 确认 LibreOffice 可执行：

   ```bash
   soffice --version
   ```

2. 使用运行服务的同一用户执行一次无头转换，确认字体、权限和 profile 目录正常：

   ```bash
   mkdir -p /tmp/rof-check/out
   printf 'hello\n' > /tmp/rof-check/input.txt
   soffice --headless --norestore --nodefault --nofirststartwizard \
     -env:UserInstallation=file:///tmp/rof-check/profile \
     --convert-to pdf --outdir /tmp/rof-check/out \
     /tmp/rof-check/input.txt
   ```

3. 确认临时目录所在磁盘有足够空间，并允许运行用户创建 `0700` 目录。
4. 如需转换中文文档，安装 CJK 字体（例如 `fonts-noto-cjk`）。
5. 需要 Markdown（`md`）转换时，确认 LibreOffice 版本不低于 `26.2`（`soffice --version`）：服务启动时会做一次 `md -> md` 探测，探测不到 CommonMark 过滤器时 `md` 不会出现在 `/api/v1/formats` 中，而不会在实际转换时才报错。
6. 生产环境建议启用 API Key，并通过 TLS 入口或网关暴露服务。

注意：systemd 的 `PATH` 与交互式 shell 可能不同。建议把 `ROF_SOFFICE_BIN` 配置为绝对路径，例如 `/usr/bin/soffice`。

## 运行用户与目录

使用独立系统用户运行服务，避免以 root 执行 LibreOffice：

```bash
sudo useradd --system --user-group \
  --home-dir /var/lib/rof-convert \
  --shell /usr/sbin/nologin \
  rof-convert

sudo install -d -m 700 -o rof-convert -g rof-convert \
  /var/lib/rof-convert/tmp

sudo install -d -m 755 -o root -g root /opt/rof-convert
sudo install -m 755 target/release/rof-convert /opt/rof-convert/rof-convert
sudo install -d -m 755 -o root -g root /opt/rof-convert/config
sudo install -m 644 config/prod.toml /opt/rof-convert/config/prod.toml
```

服务按 `ROF_ENV` 加载 `config/{ROF_ENV}.toml`，默认环境为 `dev`。生产部署在环境变量文件中设置 `ROF_ENV=prod`；也可以用 `ROF_CONFIG_FILE` 指定独立配置路径。配置文件缺失或非法会导致启动失败。

环境变量文件建议只允许 root 读取，由 systemd 在启动时注入：

```bash
sudo install -m 600 -o root -g root /dev/null /etc/rof-convert.env
```

示例内容：

```bash
ROF_ENV=prod
ROF_HOST=127.0.0.1
ROF_PORT=7654
ROF_TEMP_DIR=/var/lib/rof-convert/tmp
ROF_MAX_UPLOAD_MB=100
ROF_MAX_CONCURRENCY=16
ROF_MAX_ACTIVE_JOBS=200
ROF_JOB_TTL_SECS=3600
ROF_API_KEYS=replace-with-a-random-secret
ROF_SOFFICE_BIN=/usr/bin/soffice
ROF_LOG_FORMAT=json
ROF_DOCS_ENABLED=false
RUST_LOG=rof_convert=info
```

`ROF_HOST=127.0.0.1` 适用于同机反向代理；如果直接对外监听，使用 `0.0.0.0` 并配合防火墙或安全组。生产配置中的 `auth.api_keys` 留空，必须通过 `ROF_API_KEYS` 注入密钥；留空会关闭鉴权。

## systemd

```ini
[Unit]
Description=rof-convert document conversion service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rof-convert
Group=rof-convert
WorkingDirectory=/opt/rof-convert
EnvironmentFile=/etc/rof-convert.env
Environment=HOME=/var/lib/rof-convert
ExecStart=/opt/rof-convert/rof-convert
Restart=on-failure
RestartSec=3
TimeoutStopSec=60
KillSignal=SIGTERM
LimitNOFILE=65536

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/rof-convert

[Install]
WantedBy=multi-user.target
```

说明：

- `ReadWritePaths` 必须覆盖配置文件或环境变量中的 `storage.temp_dir` / `ROF_TEMP_DIR` 父目录。
- `PrivateTmp=true` 提供私有 `/tmp`；任务目录由 `storage.temp_dir` / `ROF_TEMP_DIR` 显式指定。
- `ProtectHome=true` 下，通过 `HOME=/var/lib/rof-convert` 给 LibreOffice 一个可写家目录。
- 如需进一步限制 CPU/内存，可使用 systemd 的 `CPUQuota`、`MemoryMax` 或容器 cgroup，但应先做容量测试。
- 停止服务会触发优雅停机：停止接收新请求、取消正在执行的任务并清理工作目录；客户端需要重新提交未完成任务。

常用命令：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rof-convert
sudo systemctl status rof-convert
sudo journalctl -u rof-convert -f
```

## 构建与安装

```bash
cargo build --release --locked
```

建议在与生产环境相同或兼容的 Linux 发行版上构建。生成的二进制依赖目标系统的 glibc 和已安装的 LibreOffice；不建议把不同发行版构建的二进制直接混用。

升级步骤：

1. 选择低峰期停止服务或摘除流量。
2. 替换 `/opt/rof-convert/rof-convert`。
3. 启动服务并等待 `/readyz` 返回 `200`。
4. 恢复流量。

当前任务状态保存在内存中，重启会丢失任务记录并取消未完成任务，无法通过重启保留排队任务。

## 反向代理

推荐在 Nginx、HAProxy、API Gateway 或 Kubernetes Ingress 后部署。Nginx 示例：

```nginx
server {
    listen 443 ssl;
    server_name convert.example.internal;

    # 略：证书配置

    client_max_body_size 60m;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Request-Id $request_id;

    proxy_read_timeout 300s;
    proxy_send_timeout 300s;
    proxy_request_buffering off;

    location / {
        proxy_pass http://127.0.0.1:7654;
    }
}
```

要点：

- `client_max_body_size` 应大于 `ROF_MAX_UPLOAD_MB` 加 multipart 开销，否则请求会在网关被拒绝。
- 网关的读写超时应大于 `ROF_REQUEST_TIMEOUT_SECS`，避免应用返回统一错误前连接已被网关断开。
- 同步转换接口（`POST /api/v1/conversions/sync`）在单请求内完成转换并流式返回文件，网关超时需覆盖“排队 + 转换 + 传输”，否则客户端可能在拿到文件前被断开。
- 透传 `X-Request-Id` 可以串联网关与应用日志；如果网关未提供，应用会自动生成。
- 不要让网关覆盖或删除 `Content-Disposition`、`X-Checksum-SHA256` 和 `X-Request-Id` 响应头。
- TLS、mTLS、OIDC 等认证可在网关层补充；应用本身只实现 API Key 鉴权。

如果启用了 CORS，应在应用层或网关层统一处理，避免两边重复添加产生冲突。应用默认不启用 CORS。

## 容器部署

项目根目录提供 [Dockerfile](../Dockerfile)，包含：

- Rust 多阶段构建，先缓存依赖层再复制源码
- LibreOffice 不使用发行版仓库的版本，而是在构建时从 The Document Foundation 官方站点下载官方 `.deb` 包，默认安装最新稳定版（Debian bookworm 自带的 7.4 没有 Markdown/CommonMark 过滤器）
- 默认使用 rsproxy Cargo 镜像与中科大 Debian 镜像，适配中国大陆网络
- 通过 `RUST_IMAGE`、`DEBIAN_IMAGE`、`CARGO_REGISTRY`、`DEBIAN_MIRROR`、`LO_MIRROR`、`LO_VERSION` 构建参数覆盖基础镜像、软件源和 LibreOffice 版本
- 非 root 用户、`tini` 作为 PID 1、Docker `HEALTHCHECK`
- 默认按生产环境构建：镜像内置 `ENV ROF_ENV=prod` 与 `ROF_DOCS_ENABLED=false`，内包含 `config/prod.toml`

构建：

```bash
# 需要登录时先 docker login
# 默认使用 rsproxy 和清华 Debian 镜像
docker build -t rof-convert:0.1.0 .

# Docker Hub 拉取慢时，可指定基础镜像代理
docker build \
  --build-arg RUST_IMAGE=<mirror>/library/rust:1.99-bookworm \
  --build-arg DEBIAN_IMAGE=<mirror>/library/debian:bookworm-slim \
  -t rof-convert:0.1.0 .

# 需要替换软件源时
docker build \
  --build-arg CARGO_REGISTRY=sparse+https://mirrors.ustc.edu.cn/crates.io-index/ \
  --build-arg DEBIAN_MIRROR=http://mirrors.ustc.edu.cn \
  -t rof-convert:0.1.0 .

# 固定 LibreOffice 版本（默认 latest，每次构建解析当时的最新稳定版）
docker build --build-arg LO_VERSION=26.8.1 -t rof-convert:0.1.0 .

# LibreOffice 官方站点较慢时改用国内镜像
docker build --build-arg LO_MIRROR=https://mirrors.aliyun.com/tdf -t rof-convert:0.1.0 .
```

运行：

```bash
docker run --rm -p 7654:7654 \
  -e ROF_API_KEYS='replace-with-a-random-secret' \
  -v rof-convert-tmp:/var/lib/rof-convert/tmp \
  rof-convert:0.1.0
```

注意：

- `LO_VERSION` 默认 `latest`，构建时会读取镜像站目录索引解析最新稳定版，因此同一份代码在不同时间构建可能得到不同的 LibreOffice 版本；需要可复现镜像时固定具体版本号。
- LibreOffice 官方包没有 `-nogui` 精简变体，镜像比使用发行版仓库时大约多几百 MB；构建过程会删除帮助、图库、模板等运行期不需要的内容。
- 官方包的 `Depends` 并未覆盖全部运行库（如 `libXinerama.so.1`、`libX11-xcb.so.1`），Dockerfile 额外显式安装了一批 X11/图形库以满足无头启动；升级 LibreOffice 后若 `soffice --version` 失败，构建会用 `ldd` 打印缺失的库，把它加入安装列表即可。
- 官方包安装后由镜像写入 `/usr/bin/soffice` 包装脚本，配置中的 `soffice_bin = "soffice"` 仍按 `PATH` 查找即可。
- 建议在 `.dockerignore` 中排除 `target/` 和 `data/`，避免构建上下文过大。项目已提供 `.dockerignore`。
- 镜像默认以 `ROF_ENV=prod` 启动，并显式设置 `ROF_DOCS_ENABLED=false`，因此不会暴露 Swagger UI 与 `/api/v1/openapi.json`；自定义配置可通过挂载文件并设置 `ROF_CONFIG_FILE` 覆盖，临时排查文档时用 `-e ROF_DOCS_ENABLED=true` 开启。
- 生产环境运行时必须通过 `ROF_API_KEYS` 注入密钥；镜像内的 `config/prod.toml` 不保存真实密钥。
- 挂载卷必须对容器内 UID `10001` 可写；必要时先修正卷权限。
- 生产环境可增加 `--read-only`、`--tmpfs /tmp`、`--cap-drop=ALL`、`--security-opt no-new-privileges` 和内存/CPU 限制。LibreOffice 需要写入 `/tmp`，`--read-only` 时必须提供可写 tmpfs。
- 不要把容器内 `/var/lib/rof-convert` 挂载为只读。

许可证提示：本项目源代码采用 Apache-2.0。镜像中安装的 LibreOffice 主要采用 MPL-2.0，并包含其他许可证组件。分发镜像、安装包或离线包时，应保留相应版权与许可证声明，并按 MPL-2.0 要求提供源码获取方式；如果修改过 MPL 覆盖的文件，还需要提供修改后的源码。LibreOffice 不适用本项目的 Apache-2.0 许可证。

## Kubernetes 建议

```yaml
containers:
  - name: rof-convert
    image: rof-convert:0.1.0
    ports:
      - containerPort: 7654
    env:
      - name: ROF_ENV
        value: prod
      - name: ROF_TEMP_DIR
        value: /var/lib/rof-convert/tmp
      - name: ROF_LOG_FORMAT
        value: json
    volumeMounts:
      - name: work
        mountPath: /var/lib/rof-convert
    livenessProbe:
      httpGet:
        path: /healthz
        port: 7654
      initialDelaySeconds: 10
      periodSeconds: 10
      timeoutSeconds: 3
      failureThreshold: 3
    readinessProbe:
      httpGet:
        path: /readyz
        port: 7654
      initialDelaySeconds: 5
      periodSeconds: 10
      timeoutSeconds: 5
      failureThreshold: 3
```

- 当前实现为**单实例状态**，不要直接设置 `replicas > 1`。创建任务、轮询和下载可能命中不同 Pod，导致任务查不到。
- Pod 需要可写的临时卷；可使用 `emptyDir`，并根据单文件上限、并发数和结果保留时间设置容量与告警。
- 镜像默认包含 `config/prod.toml`；如需用 ConfigMap 管理配置，挂载文件并设置 `ROF_CONFIG_FILE`，或在环境变量中覆盖配置项。
- `terminationGracePeriodSeconds` 建议不小于 60 秒，给优雅停机和文件清理留出时间。
- 资源 requests/limits 应根据代表性文档实测后配置，避免多个 soffice 进程触发 OOM。
- 如果使用只读根文件系统，需要给 `/tmp` 和 `ROF_TEMP_DIR` 提供可写卷。

## 容量规划

### 并发

`ROF_MAX_CONCURRENCY` 控制同时运行的 LibreOffice 进程数，默认 `16`，`ROF_MAX_ACTIVE_JOBS` 默认 `200`。LibreOffice 转换通常 CPU 密集，也可能使用较多内存：

- 并发请求数不等于并发转换数：默认组合可接受约 50 个同时提交的请求，其中 16 个立即执行，其余在内存队列排队（不占用 HTTP 连接）。
- `ROF_MAX_CONCURRENCY` 建议约等于容器 CPU limit（1 核对应 1 个 soffice），内存按 `并发数 × 单任务峰值内存` 估算；16 并发建议至少 8 GiB，生产应按代表性文档实测。
- `ROF_MAX_ACTIVE_JOBS` 建议取并发数的 4 倍左右，且不小于期望接受的并发提交数；低于该值时提交会返回 `503` 并带 `Retry-After`。
- 用生产代表性文档（大文档、复杂表格、中文排版、演示文稿）压测单次耗时、峰值 CPU 和峰值内存后再微调。
- 观察超时率与 CPU 使用率；并发数过高可能导致所有任务变慢并触发 `CONVERSION_TIMEOUT`。
- 同步转换与异步任务共享同一并发额度；同步请求过多会挤占异步任务容量，大约 50 并发提交的场景建议使用异步接口，并在网关层限制同步接口 QPS。
- 限制应同时考虑容器 CPU limit 和节点资源。

### 磁盘

临时目录会同时存在上传输入、LibreOffice profile 和转换结果。粗略上界：

```text
峰值临时空间 ≈ 单文件上限 × 同时在处理的任务数
              + 结果文件大小 × 保留期内的终态任务数
```

`ROF_MAX_ACTIVE_JOBS` 为 200、上传上限为 100 MB 时，最坏情况仅上传文件就可能接近 20 GB。建议：

- 为 `ROF_TEMP_DIR` 单独挂载磁盘或卷并监控使用率；
- 根据结果大小和业务量调整 `ROF_JOB_TTL_SECS`；
- 监控 `503` 容量错误和任务失败日志，及时扩容或限流。

### 文件描述符

LibreOffice 和字体加载会打开较多文件，建议 `LimitNOFILE=65536`，并结合 `ulimit` 或容器限制验证。

## 可观测性

### 日志

- `ROF_LOG_FORMAT=json` 适合接入 ELK、Loki、云日志等平台。
- 访问日志字段包含 `request_id`、`method`、`path`、`status`、`latency_ms`。
- 转换任务日志包含 `job_id`、来源 `request_id`、`source`、`target`、耗时和结果大小。
- 日志不记录上传文件内容，也不直接输出 API Key；转换失败时仅记录截断后的 stderr 片段。

建议采集并告警：

- `/readyz` 连续失败
- HTTP 5xx 比例
- 转换失败率和超时数量
- `503` 容量错误
- 临时目录使用率
- 进程 CPU、内存、文件描述符
- 平均转换耗时和排队时长（可从任务日志聚合）

当前版本没有内置 Prometheus `/metrics`，也没有队列深度接口；需要指标时建议先扩展代码或在网关侧统计。

### 探针

- `/healthz`：表示进程存活，并返回当前 `environment` 与版本；不检查 LibreOffice 或临时目录。
- `/readyz`：检查 `soffice` 可执行文件与临时目录写权限，失败返回 `503`，响应 `details` 说明具体依赖状态。

不要把 `/healthz` 当作依赖健康检查；依赖异常时应让 `/readyz` 失败并从流量中摘除实例。

## 安全加固

- 使用非 root 用户运行，临时目录权限 `0700`。
- 在入口层启用 TLS；内部通信也建议走受信网络。
- API Key 使用随机长值，存放在环境变量或密钥管理系统中；轮换后需要重启服务。
- 限制入口来源、目标端口和出站流量；如业务允许，限制容器外联。
- 使用 `no-new-privileges`、能力裁剪、只读根文件系统、cgroup 资源限制。
- LibreOffice 解析不可信文档，独立 profile 只解决并发锁，不是安全沙箱。处理外部上传时应在容器/虚拟机中隔离，并限制 CPU、内存、磁盘和网络。
- 生产环境保持 `ROF_DOCS_ENABLED=false`（`config/prod.toml` 默认关闭），此时 `/api/v1/docs` 与 `/api/v1/openapi.json` 均未注册，返回 `404`；需要临时排查时才在受信网络中开启。
- 健康检查路径通常应仅对集群内部开放。
- 不要将 `ROF_TEMP_DIR` 放在共享可写目录；任务目录包含用户上传内容和转换结果。

## 运维操作

### 启停与重启

```bash
sudo systemctl restart rof-convert
curl -fsS http://127.0.0.1:7654/readyz
```

服务收到 `SIGTERM` 后停止接收新请求，取消内存中的任务并清理工作目录。由于任务不持久化，重启后客户端需要重新创建任务。

### API Key 轮换

1. 生成新 Key。
2. 更新 `/etc/rof-convert.env` 的 `ROF_API_KEYS`，可同时保留旧 Key 与新 Key。
3. 重启服务。
4. 通知客户端切换完成后，再次更新并移除旧 Key。

当前不支持热加载，必须重启。多 Key 以逗号分隔，最多 32 个。

### 磁盘清理

- 优先通过任务 TTL 自动清理；终态任务过期后由后台任务删除。
- 删除不再需要的任务可调用 `DELETE /api/v1/conversions/{jobId}`。
- 不要在服务运行期间手工删除活跃任务目录；如确认无活跃任务，可在停机后清理，或依赖启动时的遗留目录清理。
- 清理前确认目录位于 `ROF_TEMP_DIR` 内，避免误删。

### 水平扩展

当前版本不能安全地直接增加副本：

- 任务记录在单实例内存中，轮询可能命中其他副本并返回 `404`。
- 结果文件在单实例本地临时目录中，其他副本无法下载。
- 仅依赖负载均衡粘性不能解决实例重启或扩缩容后的任务迁移。

需要多副本时，应先实现共享任务仓储（Redis/数据库）和共享结果存储（对象存储或共享文件系统），再增加副本。

## 故障排查

| 现象 | 排查步骤 |
|---|---|
| 启动失败，提示 `configuration error` | 检查 `ROF_ENV`、`ROF_CONFIG_FILE` 和工作目录下的 `config/{ROF_ENV}.toml`；TOML 未知字段、格式错误或参数越界都会导致启动失败 |
| `/readyz` 返回 503 | 查看 `details`；确认 `ROF_SOFFICE_BIN` 绝对路径可执行；确认配置中 `storage.temp_dir` / `ROF_TEMP_DIR` 的属主与权限；确认不是只读文件系统 |
| 创建任务返回 413 | 检查应用 `ROF_MAX_UPLOAD_MB`，以及 Nginx `client_max_body_size`、Ingress 或网关限制 |
| 转换失败，error.code=`CONVERSION_FAILED` | 查看服务日志中的 soffice 退出码和 stderr；检查文档是否损坏、是否加密、字体是否缺失 |
| 转换超时 | 检查 CPU/内存与当前并发；降低 `ROF_MAX_CONCURRENCY`；适当提高 `ROF_CONVERSION_TIMEOUT_SECS` |
| 大量 503 | 当前任务数达到 `ROF_MAX_ACTIVE_JOBS` 或并发耗尽；扩容、限流或调整参数 |
| 下载返回 404 | 任务已删除、已过 `ROF_JOB_TTL_SECS`，或服务重启导致内存记录丢失 |
| 下载返回 409 | 任务仍在排队/运行，或已失败；先查询状态，失败时查看 `error` |
| 401 Unauthorized | 检查 `ROF_API_KEYS` 是否包含该 Key，请求头是否为 `X-API-Key` 或 Bearer |
| CORS 预检失败 | 确认 `ROF_CORS_ORIGINS` 包含前端来源；不要让网关与应用重复设置冲突的 CORS 头 |
| 中文乱码或排版异常 | 安装 CJK 字体；CSV 使用 UTF-8；检查服务器 locale 与字体缓存 |
| 格式目录中没有 `md` | LibreOffice 低于 26.2 或探测失败（启动日志中的 `LibreOffice capabilities detected markdown=false`）；升级 LibreOffice 后重启 |
| `failed to start LibreOffice` | 检查二进制架构、执行权限、挂载是否 `noexec`、动态库是否齐全 |
| 临时目录权限错误 | 确认运行用户对配置中 `storage.temp_dir` / `ROF_TEMP_DIR` 有读写执行权限，目录权限为 `0700` |
| 磁盘占满 | 检查任务 TTL、异常大文件、失败残留；扩容临时卷并设置磁盘告警 |

## 升级与回滚

1. 备份当前二进制、环境变量文件和配置文件。
2. 构建新版本并执行 `cargo clippy --all-targets -- -D warnings`、`cargo test`。
3. 在预发环境用代表性文档验证主要转换路径和下载校验。
4. 低峰期替换二进制/配置并重启，等待 `/readyz` 恢复。
5. 观察错误率、转换耗时、临时磁盘和 CPU/内存。
6. 如需回滚，恢复旧二进制和配置文件并重启；未完成任务需要客户端重新提交。

配置变更、API Key 轮换和版本升级都需要重启，不能依赖热加载。
