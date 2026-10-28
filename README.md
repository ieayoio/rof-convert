# rof-convert

基于 Rust、axum 与 LibreOffice 的无头文档转换服务。服务通过异步任务模式接收上传文件，调用 `soffice` 完成文档转换，并提供任务查询、结果下载、取消删除、健康检查和 OpenAPI 契约。

接口按企业级规范设计：版本化路径、统一响应结构、请求追踪、稳定错误码、可选 API Key 鉴权、上传校验、并发控制、超时取消、结果保留与自动清理。

## 文档导航

- [架构设计](docs/architecture.md)：模块职责、任务生命周期、并发与清理机制
- [部署与运维](docs/deployment.md)：systemd、反向代理、容器、容量规划与故障排查
- [OpenAPI 契约](docs/openapi.json)：完整接口定义；运行时由 `[docs].enabled`（`ROF_DOCS_ENABLED`）控制是否暴露 `/api/v1/docs` 与 `/api/v1/openapi.json`
- 当前版本：`0.1.0`

## 功能特性

- 支持文档、表格、演示文稿的常见格式互转
- Markdown（`md`）读写：LibreOffice 26.2+ 启动探测通过后自动启用，旧版本不会对外宣称支持
- 异步任务：创建任务立即返回 `202`，通过任务状态轮询结果
- 可选同步转换：单请求内完成转换并流式返回文件，与异步任务共享并发额度
- 结果下载附带 `SHA-256` 校验值和 RFC 5987 文件名
- 统一响应结构：`success/code/message/requestId/timestamp/data/details`
- `X-Request-Id` 透传或自动生成，日志可按请求追踪
- 可选 API Key 鉴权：`X-API-Key` 或 `Authorization: Bearer`
- 文件扩展名白名单、上传大小限制、文件名清洗、路径穿越防护
- 每个任务使用独立 LibreOffice 用户配置目录，支持并发转换
- 信号量控制并发、任务超时强制结束、运行中任务可取消
- 任务结果按 TTL 自动清理，启动时清理异常退出遗留目录
- 优雅停机、结构化日志（text/json）、可配置 CORS
- 存活/就绪探针，就绪检查覆盖 `soffice` 与临时目录
- 完整 OpenAPI 3.0 契约

## 支持格式

源格式由上传文件扩展名决定，大小写不敏感。目标格式为扩展名（不含点），不能与源格式相同。

| 类别 | 源格式 | 目标格式 |
|---|---|---|
| 文档 | `doc`、`docx`、`odt`、`rtf`、`txt`、`html`、`md` | `pdf`、`docx`、`doc`、`odt`、`rtf`、`txt`、`html`、`md` |
| 表格 | `xls`、`xlsx`、`ods`、`csv` | `pdf`、`xlsx`、`xls`、`ods`、`csv`、`html` |
| 演示 | `ppt`、`pptx`、`odp` | `pdf`、`pptx`、`ppt`、`odp`、`png` |

说明：

- 目标格式列表中若包含源格式，实际请求会被拒绝。
- 当前不支持 PDF 作为源格式，也不支持宏文档（`docm/xlsm/pptm`）。
- 演示文稿转 `png` 仅导出首页。
- `md`（Markdown/CommonMark）依赖 LibreOffice 26.2 及以上版本：启动时会实际探测过滤器，不可用时 `md` 不会出现在格式目录中，相关源文件与目标格式均按不支持处理。
- 文件类型以扩展名白名单判断，不进行二进制魔数嗅探；处理不可信文件时应配合容器沙箱运行。

## 快速开始

### 环境要求

- Linux（推荐）或支持 LibreOffice 无头模式的环境
- Rust `1.85+`（项目使用 edition 2024，当前开发环境为 1.99）
- LibreOffice，确保 `soffice` 可执行；需要 Markdown 转换时要求 `26.2+`
- 可写的临时目录
- 如转换中文文档，建议安装 CJK 字体

以 Debian/Ubuntu 为例：

```bash
sudo apt-get update
sudo apt-get install -y \
  libreoffice-writer libreoffice-calc libreoffice-impress \
  fonts-noto-cjk
```

确认环境：

```bash
rustc --version
soffice --version
```

### 构建与启动

```bash
cargo build --release

# 默认加载 config/dev.toml，监听 127.0.0.1:7654
ROF_API_KEYS='replace-with-a-random-secret' \
./target/release/rof-convert
```

开发配置使用 `./data/tmp` 作为任务目录，首次启动会自动创建。默认读取 `config/dev.toml`；需要生产配置时使用：

```bash
ROF_ENV=prod \
ROF_API_KEYS='replace-with-a-random-secret' \
./target/release/rof-convert
```

生产配置使用 `/var/lib/rof-convert/tmp`，需要先按[部署与运维](docs/deployment.md)创建目录并授权。随项目提供的 dev/prod 配置默认未设置 API Key，建议通过 `ROF_API_KEYS` 注入密钥；生产环境不应在配置文件中保存密钥。

服务从进程工作目录查找 `config/{ROF_ENV}.toml`。分发二进制时请一并安装 `config/` 目录，或用 `ROF_CONFIG_FILE` 指向绝对路径的配置文件。

### 容器镜像

```bash
docker build -t rof-convert:0.1.0 .

docker run --rm -p 7654:7654 \
  -e ROF_API_KEYS='replace-with-a-random-secret' \
  -v rof-convert-tmp:/var/lib/rof-convert/tmp \
  rof-convert:0.1.0
```

- 镜像默认按生产环境运行：内置 `ROF_ENV=prod` 与 `ROF_DOCS_ENABLED=false`，Swagger UI（`/api/v1/docs`）和 `/api/v1/openapi.json` 不会暴露；临时排查时用 `-e ROF_DOCS_ENABLED=true` 开启。
- 容器内已安装 LibreOffice 与 CJK 字体，无需在宿主机安装 `soffice`。
- 构建参数、镜像源、卷权限、只读根文件系统与 Kubernetes 部署见[部署与运维](docs/deployment.md)。

### 验证服务

```bash
curl -s http://127.0.0.1:7654/healthz
curl -s http://127.0.0.1:7654/readyz
curl -s -H 'X-API-Key: replace-with-a-random-secret' \
  http://127.0.0.1:7654/api/v1/formats
```

`readyz` 返回 `503` 时，响应 `details` 会给出 `soffice` 与临时目录的检查结果。

## API 概览

业务接口统一使用 `/api/v1` 前缀。配置了 API Key（`[auth].api_keys` 或环境变量 `ROF_API_KEYS`）后，除健康检查和文档路径（仅在开启时存在）外，其余接口都需要鉴权。鉴权支持 `X-API-Key: <key>` 或 `Authorization: Bearer <key>` 两种形式。

| 方法 | 路径 | 说明 | 鉴权 |
|---|---|---|---|
| `POST` | `/api/v1/conversions` | 创建转换任务 | 业务鉴权 |
| `POST` | `/api/v1/conversions/sync` | 同步转换并直接返回文件 | 业务鉴权 |
| `GET` | `/api/v1/conversions/{jobId}` | 查询任务状态 | 业务鉴权 |
| `GET` | `/api/v1/conversions/{jobId}/download` | 下载转换结果 | 业务鉴权 |
| `DELETE` | `/api/v1/conversions/{jobId}` | 取消任务并清理数据 | 业务鉴权 |
| `GET` | `/api/v1/formats` | 查询格式矩阵与运行限制 | 业务鉴权 |
| `GET` | `/healthz` | 存活探针 | 公开 |
| `GET` | `/readyz` | 就绪探针 | 公开 |
| `GET` | `/api/v1/openapi.json` | 获取 OpenAPI 契约（仅 `docs.enabled` 开启时存在） | 公开 |
| `GET` | `/api/v1/docs` | Swagger UI 页面（仅 `docs.enabled` 开启时存在） | 公开 |

### 统一响应结构

成功与失败均返回相同外层结构：

```json
{
  "success": true,
  "code": "OK",
  "message": "success",
  "requestId": "9f8e7d6c-1234-4a3b-8c2d-0123456789ab",
  "timestamp": "2026-10-27T08:00:00Z",
  "data": {},
  "details": null
}
```

- `requestId`：请求追踪 ID，可用 `X-Request-Id` 传入；缺失或非法时由服务生成。
- `timestamp`：UTC RFC 3339 时间。
- `details`：错误时可能包含字段级补充信息，成功时省略。
- 响应头 `X-Request-Id` 与响应体中的 `requestId` 一致。

### 创建转换任务

```bash
curl -sS -X POST 'http://127.0.0.1:7654/api/v1/conversions' \
  -H 'X-API-Key: replace-with-a-random-secret' \
  -H 'X-Request-Id: demo-001' \
  -F 'file=@./report.docx' \
  -F 'targetFormat=pdf'
```

`targetFormat` 可以放在 multipart 表单字段中，也可以作为查询参数：

```text
POST /api/v1/conversions?targetFormat=pdf
```

两种方式同时提供时必须一致。任务创建成功返回 `202 Accepted`，并带有 `Location` 响应头：

```json
{
  "success": true,
  "code": "OK",
  "message": "success",
  "requestId": "demo-001",
  "timestamp": "2026-10-27T08:00:00Z",
  "data": {
    "jobId": "b8b04f99-9135-493d-b5e4-1d80b49681d6",
    "status": "QUEUED",
    "source": {
      "fileName": "report.docx",
      "format": "docx",
      "sizeBytes": 5098
    },
    "target": {
      "format": "pdf"
    },
    "result": null,
    "error": null,
    "createdAt": "2026-10-27T08:00:00Z",
    "startedAt": null,
    "completedAt": null,
    "expiresAt": null,
    "statusUrl": "/api/v1/conversions/b8b04f99-9135-493d-b5e4-1d80b49681d6",
    "downloadUrl": null
  }
}
```

### 轮询任务状态

```bash
JOB_ID=b8b04f99-9135-493d-b5e4-1d80b49681d6
curl -sS -H 'X-API-Key: replace-with-a-random-secret' \
  "http://127.0.0.1:7654/api/v1/conversions/$JOB_ID"
```

状态流转：

```text
QUEUED -> RUNNING -> SUCCEEDED
                  \-> FAILED
```

成功时 `result` 包含：

```json
{
  "result": {
    "fileName": "report.pdf",
    "contentType": "application/pdf",
    "sizeBytes": 17437,
    "sha256": "1921750cb336196c3db1a1203417364e822c691ae35981026d8b855505c5cf55"
  },
  "downloadUrl": "/api/v1/conversions/b8b04f99-9135-493d-b5e4-1d80b49681d6/download"
}
```

失败时 `error` 包含 `code` 与 `message`。任务完成后才会出现 `expiresAt`，表示结果保留截止时间。

### 下载结果

```bash
curl -fL -H 'X-API-Key: replace-with-a-random-secret' \
  -o report.pdf \
  "http://127.0.0.1:7654/api/v1/conversions/$JOB_ID/download"
```

下载响应包含：

- `Content-Disposition`：附件名，同时提供 RFC 5987 的 `filename*` 形式
- `X-Checksum-SHA256`：结果文件 SHA-256 摘要
- `Content-Type`、`Content-Length`
- `Cache-Control: private, no-store`
- `X-Content-Type-Options: nosniff`

任务尚未完成时返回 `409`，并在可能的情况下返回 `Retry-After`；任务失败时返回 `409` 并附带任务错误信息。

### 同步转换

需要在单次请求内拿到结果时，使用同步接口：

```bash
curl -fL -X POST 'http://127.0.0.1:7654/api/v1/conversions/sync?targetFormat=pdf' \
  -H 'X-API-Key: replace-with-a-random-secret' \
  -F 'file=@./report.docx' \
  -o report.pdf
```

成功时响应体即转换后的文件流，响应头与下载接口一致（`Content-Disposition`、`X-Checksum-SHA256`、`Content-Type`、`Content-Length`）。该接口与异步任务共享 `max_concurrency` 并发额度：等待许可最多 `conversion.timeout_secs`，转换本身同样受该超时约束，整个请求由 `server.request_timeout_secs` 兜底。失败时返回统一错误结构，不会创建任务记录；容量不足返回 `503` 并带 `Retry-After`。

### 取消并删除任务

```bash
curl -sS -X DELETE \
  -H 'X-API-Key: replace-with-a-random-secret' \
  "http://127.0.0.1:7654/api/v1/conversions/$JOB_ID"
```

删除会取消排队或运行中的任务、移除任务记录并尽力清理工作目录。删除后再次查询或下载返回 `404`。

### 格式矩阵

```bash
curl -sS -H 'X-API-Key: replace-with-a-random-secret' \
  http://127.0.0.1:7654/api/v1/formats
```

返回各源格式、分类、可用目标格式，以及当前实例的上传大小、并发数、转换超时和结果保留时间。

## 配置

服务支持多环境配置文件。`ROF_ENV` 未设置或为空时默认使用 `dev`，启动时读取 `config/{ROF_ENV}.toml`；也可以用 `ROF_CONFIG_FILE` 指定其他文件路径。加载优先级为：

```text
内置默认值 < config/{ROF_ENV}.toml < 环境变量
```

配置文件缺失、TOML 格式错误或包含未知字段时，服务会在启动阶段直接失败。配置修改后需要重启进程。

| 环境 | 配置文件 | 主要差异 |
|---|---|---|
| `dev` | [`config/dev.toml`](config/dev.toml) | 默认环境；监听 `127.0.0.1`，临时目录 `./data/tmp`，文本日志 |
| `prod` | [`config/prod.toml`](config/prod.toml) | 监听 `0.0.0.0`，临时目录 `/var/lib/rof-convert/tmp`，JSON 日志 |
| 自定义 | `config/{env}.toml` | 如 `staging`，复制模板并设置 `ROF_ENV=staging` |

`development`、`production` 会分别归一化为 `dev`、`prod`。环境名只允许字母、数字、`-` 和 `_`。

配置文件字段：

| 配置段 | 字段 | 对应环境变量 |
|---|---|---|
| `[server]` | `host`、`port`、`max_upload_mb`、`request_timeout_secs` | `ROF_HOST`、`ROF_PORT`、`ROF_MAX_UPLOAD_MB`、`ROF_REQUEST_TIMEOUT_SECS` |
| `[conversion]` | `soffice_bin`、`timeout_secs`、`max_concurrency`、`max_active_jobs`、`job_ttl_secs` | `ROF_SOFFICE_BIN`、`ROF_CONVERSION_TIMEOUT_SECS`、`ROF_MAX_CONCURRENCY`、`ROF_MAX_ACTIVE_JOBS`、`ROF_JOB_TTL_SECS` |
| `[storage]` | `temp_dir` | `ROF_TEMP_DIR` |
| `[auth]` | `api_keys`（字符串数组） | `ROF_API_KEYS`（逗号分隔） |
| `[cors]` | `allowed_origins`（字符串数组） | `ROF_CORS_ORIGINS`（逗号分隔） |
| `[logging]` | `format`、`filter` | `ROF_LOG_FORMAT`、`RUST_LOG` |
| `[docs]` | `enabled` | `ROF_DOCS_ENABLED` |

环境变量表：

| 变量 | 默认值 | 说明 |
|---|---|---|
| `ROF_ENV` | `dev` | 环境名，决定默认配置文件；未设置或为空时使用 `dev` |
| `ROF_CONFIG_FILE` | `config/{ROF_ENV}.toml` | 显式指定配置文件；相对路径相对于进程工作目录 |
| `ROF_HOST` | `127.0.0.1` | 监听地址，仅接受 IP 地址；生产配置为 `0.0.0.0` |
| `ROF_PORT` | `7654` | 监听端口 |
| `ROF_MAX_UPLOAD_MB` | `100` | 单文件上传上限，范围 1-1024 MB |
| `ROF_CONVERSION_TIMEOUT_SECS` | `120` | 单次 soffice 转换超时，范围 5-3600 秒 |
| `ROF_REQUEST_TIMEOUT_SECS` | `300` | HTTP 请求超时（包含上传），范围 10-3600 秒 |
| `ROF_MAX_CONCURRENCY` | `16` | 同时运行的 soffice 进程数，范围 1-64；建议约等于 CPU 核数 |
| `ROF_MAX_ACTIVE_JOBS` | `200` | 队列中及运行中的任务总数上限，需不小于并发数，最大 10000 |
| `ROF_JOB_TTL_SECS` | `3600` | 终态任务记录与结果保留时间，范围 60-604800 秒 |
| `ROF_TEMP_DIR` | `./data/tmp` | 任务工作目录；生产配置为 `/var/lib/rof-convert/tmp`；启动时创建并设置 `0700` 权限 |
| `ROF_SOFFICE_BIN` | `soffice` | soffice 可执行文件，可传 PATH 名称或绝对路径 |
| `ROF_API_KEYS` | 空 | 逗号分隔的 API Key，最多 32 个；为空时不覆盖配置文件，文件也为空则关闭鉴权 |
| `ROF_CORS_ORIGINS` | 空 | 逗号分隔的允许来源；`*` 不能与其他来源同时使用；为空时不覆盖配置文件，文件也为空则关闭 CORS |
| `ROF_LOG_FORMAT` | `text` | 日志格式：`text` 或 `json`；生产配置为 `json` |
| `ROF_DOCS_ENABLED` | 关闭 | 是否暴露 Swagger UI（`/api/v1/docs`）与 OpenAPI 契约（`/api/v1/openapi.json`）；dev 配置为开启，prod 配置为关闭 |
| `RUST_LOG` | `rof_convert=info` | 日志级别过滤，覆盖配置文件中的 `logging.filter` |

补充说明：

- 配置文件中未出现的字段使用内置默认值；内置默认值与 `dev` 配置一致。
- 配置文件按字面值解析，不执行环境变量或 `~` 展开；`temp_dir` 使用相对路径时相对于进程工作目录。
- 配置文件中 `auth.api_keys` 和 `cors.allowed_origins` 使用 TOML 数组，环境变量使用逗号分隔。
- `ROF_MAX_ACTIVE_JOBS` 是排队任务与运行任务的合计上限，超限返回 `503` 与 `Retry-After`。
- 默认 `ROF_MAX_CONCURRENCY=16`、`ROF_MAX_ACTIVE_JOBS=200` 可接受约 50 个并发提交：16 个立即转换，其余排队等待信号量；调高并发数需同步核算 CPU 与单任务峰值内存。
- `ROF_REQUEST_TIMEOUT_SECS` 约束 HTTP 请求处理，不等待转换完成；转换由 `ROF_CONVERSION_TIMEOUT_SECS` 独立控制。
- API Key 应使用足够长的随机值，生产环境建议放在密钥管理系统或环境变量文件中，不要写入仓库或 `config/prod.toml`。
- CORS 默认关闭；浏览器直连场景再按需配置。
- 文档默认关闭：未配置 `[docs].enabled` 时不会注册任何文档路径；`config/dev.toml` 开启、`config/prod.toml` 关闭。关闭时 `/api/v1/docs` 与 `/api/v1/openapi.json` 均返回统一的 `404 NOT_FOUND`。

## 错误码

| 错误码 | HTTP 状态 | 说明 |
|---|---|---|
| `INVALID_ARGUMENT` | 400 | 参数、multipart 字段或任务 ID 非法 |
| `UNAUTHORIZED` | 401 | 缺少或提供了无效 API Key |
| `NOT_FOUND` | 404 | 任务不存在、已删除或已过期 |
| `METHOD_NOT_ALLOWED` | 405 | 资源不支持该 HTTP 方法 |
| `REQUEST_TIMEOUT` | 408 | HTTP 请求处理超时 |
| `CONFLICT` | 409 | 任务尚未完成、已失败或状态冲突 |
| `PAYLOAD_TOO_LARGE` | 413 | 上传文件超过大小限制 |
| `UNSUPPORTED_MEDIA_TYPE` | 415 | Content-Type 或源文件扩展名不受支持 |
| `UNSUPPORTED_CONVERSION` | 422 | 源格式到目标格式的组合不受支持 |
| `CONVERSION_FAILED` | 500 | 转换失败；通常出现在任务 `error.code` 中 |
| `CONVERSION_TIMEOUT` | 504 | 转换超时；通常出现在任务 `error.code` 中 |
| `SERVICE_UNAVAILABLE` | 503 | 容量已满或服务未就绪 |
| `INTERNAL_ERROR` | 500 | 未预期的服务端错误 |

错误响应示例：

```json
{
  "success": false,
  "code": "UNSUPPORTED_CONVERSION",
  "message": "conversion from `docx` to `xlsx` is not supported",
  "requestId": "demo-002",
  "timestamp": "2026-10-27T08:00:00Z",
  "details": {
    "sourceFormat": "docx",
    "targetFormat": "xlsx",
    "supportedTargets": ["pdf", "doc", "odt", "rtf", "txt", "html", "md"]
  }
}
```

## 任务与数据生命周期

- 上传文件写入 `{temp_dir}/{jobId}/input.{ext}`，工作目录权限为 `0700`。
- 每个任务使用独立 `profile/` 作为 LibreOffice `UserInstallation`，避免实例锁冲突。
- 转换成功后仅保留 `out/input.{target}` 供下载；输入文件和 `profile/` 立即清理。
- 转换失败或取消时清理整个任务目录。
- 终态任务在 `ROF_JOB_TTL_SECS` 后由后台任务清理，清理间隔约 60 秒。
- 启动时会删除超过「保留时间 + 转换超时」的遗留 UUID 任务目录。
- 任务记录保存在内存中。进程重启后任务记录丢失，重启前未完成的任务会被取消并清理。
- 当前实现面向单实例部署；多副本共享任务状态需要引入外部存储，详见[部署与运维](docs/deployment.md)。

## 可观测性

- 每个请求输出访问日志，包含 `request_id`、方法、路径、状态码和耗时。
- 转换任务日志包含 `job_id`、来源请求 `request_id`、源/目标格式和耗时。
- `ROF_LOG_FORMAT=json` 可输出结构化日志，便于接入日志平台。
- `/healthz` 用于存活探测，并返回当前环境名；`/readyz` 用于就绪探测，检查 `soffice` 可执行文件与临时目录写入能力。
- 当前版本不内置 Prometheus 指标端点，可基于访问日志、任务日志和网关指标建立监控。

## 开发

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

代码结构：

```text
Dockerfile         # 多阶段生产镜像（国内构建镜像源）
.dockerignore

src/
├── main.rs        # 启动、日志、临时目录、优雅停机
├── config.rs      # 多环境配置文件、环境变量覆盖与校验
├── routes.rs      # 路由与中间件装配
├── middleware.rs  # 请求 ID、鉴权、超时、panic 兜底
├── api.rs         # HTTP 处理器、上传、轮询、下载、健康检查
├── formats.rs     # 格式矩阵、导出过滤器、文件名处理
├── engine.rs      # soffice 进程调用、超时、哈希
├── jobs.rs        # 任务模型与内存任务仓储
├── worker.rs      # 任务执行、并发控制、清理
├── error.rs       # 错误码与 HTTP 映射
├── response.rs    # 统一响应结构
└── state.rs       # 共享状态

config/
├── dev.toml       # 默认开发环境配置
└── prod.toml      # 生产环境配置

docs/
├── openapi.json   # OpenAPI 3.0 契约
├── architecture.md
└── deployment.md
```

修改 `docs/openapi.json` 后需要重新编译，运行时通过 `include_str!` 提供该文件。Swagger UI 由 `utoipa-swagger-ui` 静态资源内嵌提供（构建期无需联网），页面在 `/api/v1/docs`，直接加载 `/api/v1/openapi.json`；两者都受 `ROF_DOCS_ENABLED` 控制，生产环境默认不注册。

## 已知限制

- 任务状态和结果索引保存在单实例内存中，不支持多副本共享。
- 不提供 Range 分段下载；大文件下载建议在内网直连。
- 同步转换会占用一个请求连接和一个转换并发额度，长耗时或批量场景请使用异步接口。
- 演示文稿转 PNG 仅导出首页。
- 不支持 PDF 作为源格式。
- API Key、并发数等配置不支持热加载。
- 未实现按 API Key 或客户端的独立限流，仅通过全局并发和任务总数保护资源。

## 故障排查

| 现象 | 排查方向 |
|---|---|
| 启动失败并提示 `configuration error` | 检查 `ROF_ENV`、`ROF_CONFIG_FILE` 和工作目录下的 `config/{ROF_ENV}.toml`；TOML 未知字段或参数越界会被拒绝 |
| `/readyz` 返回 503 | 确认 `soffice` 在 PATH 中或 `ROF_SOFFICE_BIN` 正确；确认配置中的 `storage.temp_dir` / `ROF_TEMP_DIR` 对运行用户可写 |
| 创建任务返回 413 | 检查 `ROF_MAX_UPLOAD_MB`，以及反向代理/网关的请求体上限 |
| 转换失败 | 查看服务日志中的 soffice 退出码与 stderr；检查文档是否损坏、字体是否缺失 |
| 格式目录里没有 `md` | LibreOffice 低于 26.2，启动探测未发现 CommonMark 过滤器；升级 LibreOffice 后重启即可 |
| 转换超时 | 提高 `ROF_CONVERSION_TIMEOUT_SECS`，检查 CPU/内存压力，降低并发 |
| 返回 503 容量不足 | 降低请求量，调整 `ROF_MAX_CONCURRENCY`/`ROF_MAX_ACTIVE_JOBS`，或增加实例（需先解决状态共享） |
| 成功结果下载 404 | 任务已删除或超过 `ROF_JOB_TTL_SECS` 保留期 |
| 中文乱码或排版异常 | 安装 `fonts-noto-cjk` 等字体；CSV 源文件使用 UTF-8 |
| 临时目录权限错误 | 确认目录属主为运行用户，权限为 `0700`，且不是只读挂载 |

更完整的部署、容量规划和加固建议见[部署与运维](docs/deployment.md)。

## 许可证

本项目源代码采用 [Apache License 2.0](LICENSE)。

运行时会调用独立的 LibreOffice `soffice` 可执行文件。LibreOffice 主要采用 [MPL-2.0](https://www.libreoffice.org/about-us/licenses)，并包含其他第三方许可证组件；LibreOffice 不适用本项目的 Apache-2.0 许可证。如果分发包含 LibreOffice 的容器镜像、安装包或离线包，请同时遵守 LibreOffice 及其第三方组件的许可证要求，保留相应版权、许可证和源码获取说明。

Apache License 2.0 不授予 LibreOffice 名称、Logo 或商标使用权。第三方 Rust 依赖保留其各自的许可证。
