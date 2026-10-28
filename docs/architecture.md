# 架构设计

本文说明 rof-convert 的模块划分、请求与任务生命周期、并发控制、文件清理及安全边界，便于后续维护和二次开发。

## 目标与范围

服务目标是把 LibreOffice 的命令行转换能力封装成稳定的 HTTP API：

- 接收上传文件并异步执行转换，避免长请求占用连接
- 对文件格式、大小、文件名和并发资源进行限制
- 提供统一的接口契约、错误码、请求追踪和健康检查
- 对任务工作目录、进程超时、取消与过期数据进行生命周期管理

当前设计面向单实例部署。任务记录和状态保存在进程内存中，转换结果保存在本地临时目录。

## 总体结构

```text
                    HTTP
  Client  ─────────────────────▶  axum Router
                                    │
                          CORS / Request-Id / Timeout
                                    │
                                    ▼
                              API handlers
                              │           │
                     上传并创建任务      查询/下载/删除
                              │           │
                              ▼           ▼
                         ┌──────────────────┐
                         │   JobStore       │
                         │ 内存任务与状态   │
                         └────────┬─────────┘
                                  │ spawn
                                  ▼
                         ┌──────────────────┐
                         │ worker           │
                         │ 信号量 + 超时取消│
                         └────────┬─────────┘
                                  │ 独立 profile
                                  ▼
                         ┌──────────────────┐
                         │ soffice 无头进程 │
                         └──────────────────┘
```

## 模块职责

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 读取配置、初始化日志、准备临时目录、清理遗留工作区、启动 HTTP 服务、优雅停机 |
| `src/config.rs` | 多环境配置文件加载、环境变量覆盖、范围校验与默认值 |
| `src/routes.rs` | 公开接口与业务接口分层、路由注册、请求体上限、CORS |
| `src/middleware.rs` | `X-Request-Id`、访问日志、panic 兜底、API Key 鉴权、请求超时 |
| `src/response.rs` | 统一成功响应、响应上下文、无缓存与 `nosniff` 响应头 |
| `src/error.rs` | 稳定错误码、HTTP 状态映射、错误详情与 `Retry-After` |
| `src/api.rs` | multipart 上传、格式校验、任务创建、状态查询、下载、删除、探针与格式目录 |
| `src/formats.rs` | 源/目标格式矩阵、LibreOffice 导出过滤器、能力开关、文件名清洗、MIME 与展示文件名 |
| `src/jobs.rs` | 任务模型、状态迁移、内存任务仓储、TTL 过期移除、任务视图 DTO |
| `src/worker.rs` | 任务排队、并发信号量、调用引擎、成功/失败落库、工作区清理、后台清理任务 |
| `src/engine.rs` | 组装 soffice 参数、独立 profile、超时与取消、输出文件发现、SHA-256 计算、启动能力探测 |
| `src/state.rs` | 共享 `Config`、`JobStore`、并发信号量和启动时间 |

## 配置加载

服务支持多环境配置，默认环境为 `dev`：

- `ROF_ENV` 未设置或为空时使用 `dev`；`development`/`production` 会归一化为 `dev`/`prod`。
- 默认配置文件为 `config/{ROF_ENV}.toml`；`ROF_CONFIG_FILE` 可以直接指定其他路径。
- 配置优先级为：内置默认值 < 配置文件 < 环境变量。
- 配置文件缺失、TOML 非法或包含未知字段时，启动阶段直接失败。
- `config/dev.toml` 使用 `127.0.0.1`、`./data/tmp` 和文本日志；`config/prod.toml` 使用 `0.0.0.0`、`/var/lib/rof-convert/tmp` 和 JSON 日志。
- `[auth]`、`[cors]` 和 `[logging]` 等敏感或部署相关值可通过环境变量覆盖；密钥不应写入生产配置文件。
- 内置默认值与 `dev` 配置一致。配置在启动时加载，修改后需要重启进程。
- 启动日志记录 `environment` 与 `config_file`；`/healthz` 返回当前环境名，便于确认实际生效环境。

配置文件字段与 `Config` 结构一一对应，未知字段会被拒绝，避免拼写错误被静默忽略。环境变量 `RUST_LOG` 优先于文件中的 `logging.filter`。

## 请求处理流程

### 中间件顺序

外层到内层大致为：

```text
CORS
  -> Request-Id / 访问日志 / panic 兜底
    -> HTTP 请求超时
      -> API Key 鉴权（仅 /api/v1 业务接口）
        -> Body 上限
          -> handler
```

- `request_context` 最外层之一，确保超时、鉴权和 panic 响应也能带上 `X-Request-Id`。
- CORS 位于最外层，OPTIONS 预检可以在鉴权前返回。
- 健康检查、Swagger UI（`/docs`，兼容 `/api/v1/docs`）与 `/api/v1/openapi.json` 不使用 API Key 鉴权；其他 `/api/v1` 接口受保护。
- 文档路由受 `[docs].enabled`（`ROF_DOCS_ENABLED`）控制，关闭时完全不注册，不经过鉴权也不会返回 `401`。
- panic 被请求上下文捕获，返回统一 `INTERNAL_ERROR`，避免连接直接断开。

### 创建转换任务

1. 检查当前排队与运行任务总数，超过 `ROF_MAX_ACTIVE_JOBS` 返回 `503`。
2. 根据 `Content-Length` 预检请求体，拒绝明显超限的请求。
3. 确认 `Content-Type` 为 `multipart/form-data`。
4. 读取 `file` 与 `targetFormat`；源格式由文件名扩展名决定。
5. 目标格式已知时，在写入完整文件前校验源/目标组合。
6. 创建 `{temp_dir}/{jobId}`，权限 `0700`，流式写入输入文件并累计大小。
7. 校验目标格式、获取 LibreOffice 导出过滤器。
8. 创建任务、写入内存任务仓储、返回 `202` 与 `Location`。
9. 异步 worker 开始排队执行。

上传是流式写入磁盘的，不会把整个文件读入内存。超过 `ROF_MAX_UPLOAD_MB` 立即中止并清理工作目录。

### 执行转换任务

1. worker 等待 `ROF_MAX_CONCURRENCY` 信号量许可；排队期间可被删除操作取消。
2. 任务状态从 `QUEUED` 迁移到 `RUNNING`。
3. 在任务目录下创建 `out/` 与 `profile/`，两者权限均为 `0700`。
4. 以 `-env:UserInstallation=file://.../profile` 启动独立 soffice 实例。
5. 同时等待进程输出、转换超时和取消信号：
   - 正常退出：检查输出文件是否存在且非空
   - 超时：终止进程，任务失败，错误码 `CONVERSION_TIMEOUT`
   - 取消：终止进程并清理目录
6. 对输出文件计算 SHA-256 和大小，写入任务结果。
7. 成功后只保留 `out/` 下的结果文件，删除输入文件和 profile。
8. 失败、取消或记录被删除时，清理整个任务目录。
9. 释放并发信号量。

soffice 的 stdout 被丢弃，stderr 仅在日志中保留截断片段；不会把内部 stderr 直接返回给客户端。

### 查询、下载与删除

- 查询返回任务快照，包括状态、源/目标信息、结果元数据、错误信息和过期时间。
- 下载只在 `SUCCEEDED` 时打开结果文件，并以流式响应返回；文件打开失败返回 `INTERNAL_ERROR`。
- 下载响应带 `Content-Disposition`、`Content-Length`、`Content-Type`、`X-Checksum-SHA256` 和无缓存头。
- 删除先取消任务，再从内存仓储移除记录，并异步清理工作目录；删除后的查询和下载返回 `404`。
- 结果在任务终态后保留 `ROF_JOB_TTL_SECS`；后台清理任务约每 60 秒扫描一次。

## 任务状态机

```text
                 DELETE
   QUEUED ─────────────────▶ 记录移除
     │
     │ worker 获取信号量
     ▼
   RUNNING ──── 成功 ──────▶ SUCCEEDED
     │
     ├──────── 失败 ──────▶ FAILED
     │
     └──────── DELETE ─────▶ 记录移除（取消运行中进程）
```

- `SUCCEEDED` 与 `FAILED` 为终态。
- `FAILED` 任务仍可查询错误信息，但没有可下载结果。
- 删除操作会立即移除内存记录；运行中的进程通过取消令牌终止。
- 任务记录只存在内存中，进程重启后丢失；重启时正在执行的任务会被取消。

## 并发与资源控制

| 机制 | 作用 |
|---|---|
| `ROF_MAX_CONCURRENCY` | 限制同时运行的 soffice 数量（默认 `16`），核心资源保护 |
| `ROF_MAX_ACTIVE_JOBS` | 限制排队 + 运行任务总数（默认 `200`），防止请求无限堆积 |
| `ROF_MAX_UPLOAD_MB` | 限制单文件大小，同时有请求体上限作为兜底 |
| `ROF_CONVERSION_TIMEOUT_SECS` | 限制单次 soffice 执行时间 |
| `ROF_REQUEST_TIMEOUT_SECS` | 限制 HTTP 请求处理时间，包含上传 |
| `ROF_JOB_TTL_SECS` | 限制终态任务与结果文件的保留时间 |
| 每任务独立 profile | 避免 LibreOffice 单实例锁导致并发互相阻塞 |

容量限制是入口处的软上限：极端并发下可能短暂超过 `ROF_MAX_ACTIVE_JOBS`，但并发执行的 soffice 数量始终受信号量约束。

“并发请求数”与“并发转换数”是两个层级：默认 `max_concurrency=16` / `max_active_jobs=200` 的组合可接受约 50 个同时提交的请求，其中 16 个立即执行、其余在内存队列排队等待信号量，排队不占用 HTTP 连接。需要提高同时转换的数量时调 `ROF_MAX_CONCURRENCY`，并按 `≈ CPU 核数` 与单任务峰值内存换算资源。

## 文件布局与清理

```text
{temp_dir}/
└── {jobId}/
    ├── input.docx
    ├── profile/
    └── out/
        └── input.pdf
```

| 路径 | 生命周期 |
|---|---|
| 任务根目录 | 创建时 `0700`；删除、失败、取消或过期时移除 |
| `input.{ext}` | 上传时创建；转换成功后删除，失败/取消时随目录清理 |
| `profile/` | 转换前创建；转换结束后删除 |
| `out/input.{target}` | 成功结果；保留到任务过期或被删除 |

清理机制：

1. **即时清理**：成功任务删除输入与 profile；失败/取消删除整个任务目录。
2. **TTL 清理**：后台任务删除终态且超过保留期的记录与目录。
3. **启动清理**：扫描临时根目录下的 UUID 目录，删除修改时间超过「任务 TTL + 转换超时」的遗留目录。
4. **停机清理**：优雅停机时取消内存中的任务并清理其工作目录。

非 UUID 名称的目录不会被启动清理误删，便于在共享临时根目录中与其他文件共存。

## 错误模型

`AppError` 由稳定错误码、消息、可选详情和可选 `Retry-After` 组成。`ApiCtx` 负责把请求 ID 写入统一响应体，`ErrorCode` 负责 HTTP 状态映射。错误响应不会暴露内部路径和 stderr；转换错误只保留经过截断的日志。

任务错误与 HTTP 错误的关系：

- 创建/上传/查询阶段的错误直接返回对应 HTTP 状态。
- 转换执行阶段的错误写入任务 `error` 字段，状态接口仍返回 `200`。
- 下载未完成或失败的任务返回 `409`，并从任务错误中生成安全详情。

## 安全设计

- 源文件扩展名白名单；目标格式必须属于该源格式的可选集合。
- 内部文件名固定为 `input.{ext}` / `input.{target}`，不使用客户端文件名拼接路径。
- 客户端文件名只用于展示和下载名，控制字符、路径分隔符和危险字符会被替换。
- 上传流式限流，请求体上限作为兜底。
- API Key 使用常量时间比较；支持 `X-API-Key` 与 Bearer Token。
- API Key 不写日志；错误响应不回显密钥。
- 下载设置 `Cache-Control: private, no-store` 和 `X-Content-Type-Options: nosniff`。
- 任务目录与 profile 目录使用 `0700` 权限，降低同机其他用户读取中间文件的风险。
- 调用 soffice 使用参数数组而非 shell 拼接，避免命令注入。
- 独立 profile 解决并发锁问题，但不等同于安全沙箱；处理不可信文档时应在容器/隔离环境中运行，并限制 CPU、内存、文件和网络资源。

## 设计取舍

### 默认异步任务，可选同步转换

LibreOffice 转换可能耗时数十秒，同步 HTTP 会占用连接、增加网关超时风险，因此默认接口采用异步任务：快速返回 `202`，并通过轮询获取结果。代价是客户端需要轮询，且当前没有回调/WebSocket 通知。

为兼顾低延迟的单文件场景，另提供 `POST /api/v1/conversions/sync`：在单次请求内完成转换并流式返回文件。该接口与异步任务共享 `max_concurrency` 信号量，等待许可与转换分别以 `conversion_timeout` 为上限，整个请求还受 `request_timeout` 兜底；转换失败直接返回统一错误结构，不写入任务仓储。批量或长耗时场景仍应使用异步接口。

### 内存任务仓储而不是数据库

当前实现优先保持单二进制、无外部依赖。重启后任务记录丢失，多副本无法共享状态。若需要水平扩展，应将 `JobStore` 抽象为外部存储，并将结果文件放入共享对象存储。

### 扩展名白名单而不是内容嗅探

LibreOffice 支持大量格式，企业场景先限制常用格式更易控。当前仅依据扩展名判断类型，未校验 ZIP/OLE 等文件签名；不可信文件应配合沙箱。

### 显式导出过滤器

每种源类型使用明确的 LibreOffice 导出过滤器（如 `writer_pdf_Export`、`Calc MS Excel 2007 XML`），避免依赖默认过滤器带来的行为差异。

### 启动时探测格式能力

部分格式依赖较新的 LibreOffice 过滤器：Markdown（CommonMark）的导入与导出自 26.2 起才原生提供，Debian bookworm 等发行版自带的 7.4 没有该过滤器。若在格式矩阵中无条件声明 `md`，旧环境下调用方会拿到 `CONVERSION_FAILED`，而非可预期的 4xx。

因此启动时用一份最小样例做一次 `md -> md` 往返探测（同时覆盖导入与导出），结果存入 `formats::Capabilities` 并随 `AppState` 下发：

- 探测失败或超时（上限 30 秒）则视为不支持，`md` 不进入 `/api/v1/formats`
- `.md` 上传返回 `415 UNSUPPORTED_MEDIA_TYPE`，转 `md` 返回 `422 UNSUPPORTED_CONVERSION`，`details` 中的候选列表同样不含 `md`
- 升级 LibreOffice 后只需重启服务，无需改配置

## 已知限制与扩展方向

当前限制：

- 任务状态与索引仅存内存，单实例
- 无 Range 分段下载
- 演示文稿转 PNG 只导出首页
- 不支持 PDF 作为源格式
- 无 Prometheus 指标
- 配置不支持热加载
- 无按 API Key/客户端的独立限流

可扩展方向：

- 将 `JobStore` 替换为 Redis/数据库，结果文件写入对象存储，支持多副本
- 引入消息队列解耦上传与转换 worker
- 增加转换完成回调、Webhook 或事件通知
- 增加指标、审计日志和按租户限流
- 通过多页导出或 PDF 拆页支持演示文稿批量 PNG
- 使用文件签名校验和病毒扫描增强上传安全
