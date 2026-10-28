//! 服务配置：支持 dev/prod 等多环境配置文件，并允许环境变量覆盖。
//!
//! 加载顺序（后面的来源优先级更高）：
//! 1. 内置默认值（与 `config/dev.toml` 保持一致）
//! 2. `config/{ROF_ENV}.toml`
//! 3. 环境变量
//!
//! `ROF_ENV` 未设置或为空时默认为 `dev`；`ROF_CONFIG_FILE` 可直接指定配置文件路径。
//! 配置文件字段缺失时使用内置默认值，未知字段会导致启动失败。
//!
//! 环境变量：
//! - `ROF_ENV`（默认 `dev`；`development`/`production` 会归一化为 `dev`/`prod`）
//! - `ROF_CONFIG_FILE`（默认 `config/{ROF_ENV}.toml`）
//! - `ROF_HOST`（默认 `127.0.0.1`）、`ROF_PORT`（默认 `8080`）
//! - `ROF_MAX_UPLOAD_MB`（默认 `50`）
//! - `ROF_CONVERSION_TIMEOUT_SECS`（默认 `120`）
//! - `ROF_REQUEST_TIMEOUT_SECS`（默认 `300`）
//! - `ROF_MAX_CONCURRENCY`（默认 `2`）、`ROF_MAX_ACTIVE_JOBS`（默认 `100`）
//! - `ROF_JOB_TTL_SECS`（默认 `3600`）
//! - `ROF_TEMP_DIR`（默认 `./data/tmp`）
//! - `ROF_SOFFICE_BIN`（默认 `soffice`）
//! - `ROF_API_KEYS`（逗号分隔；未设置或为空时不覆盖配置文件，最终为空则关闭鉴权）
//! - `ROF_CORS_ORIGINS`（逗号分隔；未设置或为空时不覆盖配置文件，最终为空则关闭 CORS）
//! - `ROF_LOG_FORMAT`（`text` 或 `json`）
//! - `ROF_DOCS_ENABLED`（`true`/`false`；未设置时取配置文件，默认关闭）
//! - `RUST_LOG` 覆盖配置文件中的日志过滤表达式

use std::{
    env,
    fmt::Display,
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use axum::http::HeaderValue;
use serde::Deserialize;
use thiserror::Error;

const MIB: usize = 1024 * 1024;
const DEFAULT_ENVIRONMENT: &str = "dev";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MAX_UPLOAD_MB: usize = 50;
const DEFAULT_CONVERSION_TIMEOUT_SECS: u64 = 120;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_CONCURRENCY: usize = 2;
const DEFAULT_MAX_ACTIVE_JOBS: usize = 100;
const DEFAULT_JOB_TTL_SECS: u64 = 3600;
const DEFAULT_TEMP_DIR: &str = "./data/tmp";
const DEFAULT_SOFFICE_BIN: &str = "soffice";
const DEFAULT_LOG_FORMAT: &str = "text";
const DEFAULT_LOG_FILTER: &str = "rof_convert=info";
const DEFAULT_DOCS_ENABLED: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: String,
    pub config_file: PathBuf,
    pub bind_addr: SocketAddr,
    pub max_upload_bytes: usize,
    pub conversion_timeout: Duration,
    pub request_timeout: Duration,
    pub max_concurrency: usize,
    pub max_active_jobs: usize,
    pub job_ttl: Duration,
    pub temp_dir: PathBuf,
    pub soffice_bin: String,
    pub api_keys: Vec<String>,
    pub cors_origins: Vec<String>,
    pub log_format: LogFormat,
    pub log_filter: String,
    /// 是否暴露 Swagger UI 页面与 `/api/v1/openapi.json` 契约。
    pub docs_enabled: bool,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let environment = resolve_environment()?;
        let config_file = resolve_config_file(&environment);
        let file_patch = read_config_file(&config_file)?;
        let environment_patch = env_patch()?;
        Self::resolve(environment, config_file, file_patch, environment_patch)
    }

    fn resolve(
        environment: String,
        config_file: PathBuf,
        file: ConfigPatch,
        environment_patch: ConfigPatch,
    ) -> Result<Self, ConfigError> {
        let host_raw = pick(
            environment_patch.server.host,
            file.server.host,
            DEFAULT_HOST.to_string(),
        );
        let host = host_raw.parse::<IpAddr>().map_err(|error| {
            ConfigError::invalid(
                "server.host",
                &host_raw,
                format!("failed to parse IP address: {error}"),
            )
        })?;
        let port = pick(
            environment_patch.server.port,
            file.server.port,
            DEFAULT_PORT,
        );
        let max_upload_mb = pick(
            environment_patch.server.max_upload_mb,
            file.server.max_upload_mb,
            DEFAULT_MAX_UPLOAD_MB,
        );
        let request_timeout_secs = pick(
            environment_patch.server.request_timeout_secs,
            file.server.request_timeout_secs,
            DEFAULT_REQUEST_TIMEOUT_SECS,
        );
        let conversion_timeout_secs = pick(
            environment_patch.conversion.timeout_secs,
            file.conversion.timeout_secs,
            DEFAULT_CONVERSION_TIMEOUT_SECS,
        );
        let max_concurrency = pick(
            environment_patch.conversion.max_concurrency,
            file.conversion.max_concurrency,
            DEFAULT_MAX_CONCURRENCY,
        );
        let max_active_jobs = pick(
            environment_patch.conversion.max_active_jobs,
            file.conversion.max_active_jobs,
            DEFAULT_MAX_ACTIVE_JOBS,
        );
        let job_ttl_secs = pick(
            environment_patch.conversion.job_ttl_secs,
            file.conversion.job_ttl_secs,
            DEFAULT_JOB_TTL_SECS,
        );
        let temp_dir_raw = pick(
            environment_patch.storage.temp_dir,
            file.storage.temp_dir,
            DEFAULT_TEMP_DIR.to_string(),
        );
        let soffice_bin = pick(
            environment_patch.conversion.soffice_bin,
            file.conversion.soffice_bin,
            DEFAULT_SOFFICE_BIN.to_string(),
        );
        let api_keys = normalize_api_keys(pick(
            environment_patch.auth.api_keys,
            file.auth.api_keys,
            Vec::new(),
        ))?;
        let cors_origins = normalize_cors_origins(pick(
            environment_patch.cors.allowed_origins,
            file.cors.allowed_origins,
            Vec::new(),
        ))?;
        let log_format_raw = pick(
            environment_patch.logging.format,
            file.logging.format,
            DEFAULT_LOG_FORMAT.to_string(),
        );
        let log_format = parse_log_format(&log_format_raw)?;
        let log_filter = pick(
            environment_patch.logging.filter,
            file.logging.filter,
            DEFAULT_LOG_FILTER.to_string(),
        )
        .trim()
        .to_string();
        let docs_enabled = pick(
            environment_patch.docs.enabled,
            file.docs.enabled,
            DEFAULT_DOCS_ENABLED,
        );

        if !(1..=1024).contains(&max_upload_mb) {
            return Err(ConfigError::invalid(
                "server.max_upload_mb",
                max_upload_mb.to_string(),
                "must be between 1 and 1024",
            ));
        }
        let max_upload_bytes = max_upload_mb.checked_mul(MIB).ok_or_else(|| {
            ConfigError::invalid(
                "server.max_upload_mb",
                max_upload_mb.to_string(),
                "value is too large",
            )
        })?;
        if !(5..=3600).contains(&conversion_timeout_secs) {
            return Err(ConfigError::invalid(
                "conversion.timeout_secs",
                conversion_timeout_secs.to_string(),
                "must be between 5 and 3600",
            ));
        }
        if !(10..=3600).contains(&request_timeout_secs) {
            return Err(ConfigError::invalid(
                "server.request_timeout_secs",
                request_timeout_secs.to_string(),
                "must be between 10 and 3600",
            ));
        }
        if !(1..=64).contains(&max_concurrency) {
            return Err(ConfigError::invalid(
                "conversion.max_concurrency",
                max_concurrency.to_string(),
                "must be between 1 and 64",
            ));
        }
        if max_active_jobs < max_concurrency || max_active_jobs > 10_000 {
            return Err(ConfigError::invalid(
                "conversion.max_active_jobs",
                max_active_jobs.to_string(),
                "must be between conversion.max_concurrency and 10000",
            ));
        }
        if !(60..=604_800).contains(&job_ttl_secs) {
            return Err(ConfigError::invalid(
                "conversion.job_ttl_secs",
                job_ttl_secs.to_string(),
                "must be between 60 and 604800",
            ));
        }
        let temp_dir_raw = temp_dir_raw.trim();
        if temp_dir_raw.is_empty() {
            return Err(ConfigError::invalid(
                "storage.temp_dir",
                "",
                "must not be empty",
            ));
        }
        let soffice_bin = soffice_bin.trim();
        if soffice_bin.is_empty() {
            return Err(ConfigError::invalid(
                "conversion.soffice_bin",
                "",
                "must not be empty",
            ));
        }
        if log_filter.is_empty() {
            return Err(ConfigError::invalid(
                "logging.filter",
                "",
                "must not be empty",
            ));
        }

        Ok(Self {
            environment,
            config_file,
            bind_addr: SocketAddr::new(host, port),
            max_upload_bytes,
            conversion_timeout: Duration::from_secs(conversion_timeout_secs),
            request_timeout: Duration::from_secs(request_timeout_secs),
            max_concurrency,
            max_active_jobs,
            job_ttl: Duration::from_secs(job_ttl_secs),
            temp_dir: PathBuf::from(temp_dir_raw),
            soffice_bin: soffice_bin.to_string(),
            api_keys,
            cors_origins,
            log_format,
            log_filter,
            docs_enabled,
        })
    }

    pub fn auth_enabled(&self) -> bool {
        !self.api_keys.is_empty()
    }

    pub fn max_upload_mb(&self) -> usize {
        self.max_upload_bytes / MIB
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read configuration file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse configuration file `{path}`: {source}")]
    ParseFile {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid configuration `{name}`={value:?}: {reason}")]
    Invalid {
        name: &'static str,
        value: String,
        reason: String,
    },
}

impl ConfigError {
    fn invalid(name: &'static str, value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            name,
            value: value.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigPatch {
    server: ServerPatch,
    conversion: ConversionPatch,
    storage: StoragePatch,
    auth: AuthPatch,
    cors: CorsPatch,
    logging: LoggingPatch,
    docs: DocsPatch,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerPatch {
    host: Option<String>,
    port: Option<u16>,
    max_upload_mb: Option<usize>,
    request_timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConversionPatch {
    soffice_bin: Option<String>,
    timeout_secs: Option<u64>,
    max_concurrency: Option<usize>,
    max_active_jobs: Option<usize>,
    job_ttl_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StoragePatch {
    temp_dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AuthPatch {
    api_keys: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CorsPatch {
    allowed_origins: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LoggingPatch {
    format: Option<String>,
    filter: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DocsPatch {
    enabled: Option<bool>,
}

fn pick<T>(environment: Option<T>, file: Option<T>, default: T) -> T {
    environment.or(file).unwrap_or(default)
}

fn resolve_environment() -> Result<String, ConfigError> {
    let raw = env_opt("ROF_ENV").unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string());
    normalize_environment(&raw)
}

fn normalize_environment(raw: &str) -> Result<String, ConfigError> {
    let normalized = raw.trim().to_ascii_lowercase();
    let normalized = match normalized.as_str() {
        "development" => "dev",
        "production" => "prod",
        other => other,
    };
    if normalized.is_empty()
        || normalized.len() > 32
        || !normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ConfigError::invalid(
            "ROF_ENV",
            raw,
            "must contain only letters, digits, '-' or '_' (max 32 characters)",
        ));
    }
    Ok(normalized.to_string())
}

fn resolve_config_file(environment: &str) -> PathBuf {
    env_opt("ROF_CONFIG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config").join(format!("{environment}.toml")))
}

fn read_config_file(path: &Path) -> Result<ConfigPatch, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&contents).map_err(|source| ConfigError::ParseFile {
        path: path.to_path_buf(),
        source,
    })
}

fn env_patch() -> Result<ConfigPatch, ConfigError> {
    let mut patch = ConfigPatch::default();
    patch.server.host = env_opt("ROF_HOST");
    patch.server.port = parse_env_opt("ROF_PORT")?;
    patch.server.max_upload_mb = parse_env_opt("ROF_MAX_UPLOAD_MB")?;
    patch.server.request_timeout_secs = parse_env_opt("ROF_REQUEST_TIMEOUT_SECS")?;
    patch.conversion.soffice_bin = env_opt("ROF_SOFFICE_BIN");
    patch.conversion.timeout_secs = parse_env_opt("ROF_CONVERSION_TIMEOUT_SECS")?;
    patch.conversion.max_concurrency = parse_env_opt("ROF_MAX_CONCURRENCY")?;
    patch.conversion.max_active_jobs = parse_env_opt("ROF_MAX_ACTIVE_JOBS")?;
    patch.conversion.job_ttl_secs = parse_env_opt("ROF_JOB_TTL_SECS")?;
    patch.storage.temp_dir = env_opt("ROF_TEMP_DIR");
    patch.auth.api_keys = env_opt("ROF_API_KEYS").map(|raw| split_csv(&raw));
    patch.cors.allowed_origins = env_opt("ROF_CORS_ORIGINS").map(|raw| split_csv(&raw));
    patch.logging.format = env_opt("ROF_LOG_FORMAT");
    patch.logging.filter = env_opt("RUST_LOG");
    patch.docs.enabled = parse_env_bool("ROF_DOCS_ENABLED")?;
    Ok(patch)
}

fn env_opt(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_env_opt<T>(name: &'static str) -> Result<Option<T>, ConfigError>
where
    T: FromStr,
    T::Err: Display,
{
    match env_opt(name) {
        Some(raw) => raw.parse().map(Some).map_err(|error| {
            ConfigError::invalid(name, raw, format!("failed to parse value: {error}"))
        }),
        None => Ok(None),
    }
}

fn parse_env_bool(name: &'static str) -> Result<Option<bool>, ConfigError> {
    match env_opt(name) {
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Some(true)),
            "false" | "0" | "no" | "off" => Ok(Some(false)),
            _ => Err(ConfigError::invalid(
                name,
                raw,
                "expected `true` or `false`",
            )),
        },
        None => Ok(None),
    }
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_api_keys(keys: Vec<String>) -> Result<Vec<String>, ConfigError> {
    let keys: Vec<String> = keys
        .into_iter()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect();
    if keys.len() > 32 {
        return Err(ConfigError::invalid(
            "auth.api_keys",
            format!("{} keys", keys.len()),
            "at most 32 API keys are supported",
        ));
    }
    for key in &keys {
        if HeaderValue::from_str(key).is_err() {
            return Err(ConfigError::invalid(
                "auth.api_keys",
                "<redacted>",
                "API keys must be valid ASCII header values",
            ));
        }
    }
    Ok(keys)
}

fn normalize_cors_origins(origins: Vec<String>) -> Result<Vec<String>, ConfigError> {
    let origins: Vec<String> = origins
        .into_iter()
        .map(|origin| origin.trim().to_string())
        .filter(|origin| !origin.is_empty())
        .collect();
    if origins.iter().any(|origin| origin == "*") && origins.len() > 1 {
        return Err(ConfigError::invalid(
            "cors.allowed_origins",
            origins.join(","),
            "`*` cannot be combined with explicit origins",
        ));
    }
    for origin in origins.iter().filter(|origin| origin.as_str() != "*") {
        HeaderValue::from_str(origin).map_err(|error| {
            ConfigError::invalid(
                "cors.allowed_origins",
                origin,
                format!("invalid origin header value: {error}"),
            )
        })?;
    }
    Ok(origins)
}

fn parse_log_format(raw: &str) -> Result<LogFormat, ConfigError> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => Err(ConfigError::invalid(
            "logging.format",
            raw,
            "expected `text` or `json`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_profiles_are_valid() {
        for environment in ["dev", "prod"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("config")
                .join(format!("{environment}.toml"));
            let file = read_config_file(&path).expect("bundled config should parse");
            let config = Config::resolve(
                environment.to_string(),
                path.clone(),
                file,
                ConfigPatch::default(),
            )
            .expect("bundled config should validate");
            assert_eq!(config.environment, environment);
            assert_eq!(config.config_file, path);
        }
    }

    #[test]
    fn environment_values_override_file_values() {
        let file: ConfigPatch = toml::from_str(
            r#"
            [server]
            host = "127.0.0.1"
            port = 8081

            [conversion]
            max_concurrency = 2
            max_active_jobs = 10

            [storage]
            temp_dir = "./file-tmp"

            [logging]
            format = "text"
            filter = "rof_convert=info"
            "#,
        )
        .expect("test config should parse");

        let mut environment_patch = ConfigPatch::default();
        environment_patch.server.host = Some("0.0.0.0".to_string());
        environment_patch.server.port = Some(9090);
        environment_patch.conversion.max_concurrency = Some(3);
        environment_patch.conversion.max_active_jobs = Some(30);
        environment_patch.storage.temp_dir = Some("./env-tmp".to_string());
        environment_patch.logging.format = Some("json".to_string());
        environment_patch.logging.filter = Some("debug".to_string());
        environment_patch.auth.api_keys = Some(vec!["env-key".to_string()]);

        let config = Config::resolve(
            "dev".to_string(),
            PathBuf::from("config/dev.toml"),
            file,
            environment_patch,
        )
        .expect("merged config should validate");

        assert_eq!(config.bind_addr, SocketAddr::from(([0, 0, 0, 0], 9090)));
        assert_eq!(config.max_concurrency, 3);
        assert_eq!(config.max_active_jobs, 30);
        assert_eq!(config.temp_dir, PathBuf::from("./env-tmp"));
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.log_filter, "debug");
        assert_eq!(config.api_keys, vec!["env-key".to_string()]);
    }

    #[test]
    fn docs_are_disabled_by_default() {
        let config = Config::resolve(
            "staging".to_string(),
            PathBuf::from("config/staging.toml"),
            ConfigPatch::default(),
            ConfigPatch::default(),
        )
        .expect("defaults should validate");
        assert!(!config.docs_enabled);
    }

    #[test]
    fn docs_follow_configuration_and_environment_overrides() {
        let mut file = ConfigPatch::default();
        file.docs.enabled = Some(true);
        let config = Config::resolve(
            "dev".to_string(),
            PathBuf::from("config/dev.toml"),
            file,
            ConfigPatch::default(),
        )
        .expect("file docs flag should validate");
        assert!(config.docs_enabled);

        let mut environment_patch = ConfigPatch::default();
        environment_patch.docs.enabled = Some(false);
        let config = Config::resolve(
            "dev".to_string(),
            PathBuf::from("config/dev.toml"),
            ConfigPatch {
                docs: DocsPatch {
                    enabled: Some(true),
                },
                ..ConfigPatch::default()
            },
            environment_patch,
        )
        .expect("environment docs flag should validate");
        assert!(!config.docs_enabled);
    }

    #[test]
    fn normalizes_environment_names() {
        assert_eq!(normalize_environment("development").unwrap(), "dev");
        assert_eq!(normalize_environment("PRODUCTION").unwrap(), "prod");
        assert!(normalize_environment("../prod").is_err());
        assert!(normalize_environment("").is_err());
    }

    #[test]
    fn rejects_unknown_config_fields() {
        let result: Result<ConfigPatch, _> = toml::from_str("[server]\nunknown = true\n");
        assert!(result.is_err());
    }

    #[test]
    fn defaults_are_valid() {
        let config = Config {
            environment: "dev".to_string(),
            config_file: PathBuf::from("config/dev.toml"),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            max_upload_bytes: 50 * MIB,
            conversion_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(300),
            max_concurrency: 2,
            max_active_jobs: 100,
            job_ttl: Duration::from_secs(3600),
            temp_dir: PathBuf::from("./data/tmp"),
            soffice_bin: "soffice".to_string(),
            api_keys: Vec::new(),
            cors_origins: Vec::new(),
            log_format: LogFormat::Text,
            log_filter: "rof_convert=info".to_string(),
            docs_enabled: false,
        };
        assert_eq!(config.max_upload_mb(), 50);
        assert!(!config.auth_enabled());
        assert!(!config.docs_enabled);
    }
}
