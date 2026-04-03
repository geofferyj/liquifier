pub mod chains;

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

pub use chains::{
    chain_name_to_id, enabled_chains, get_chain, BaseToken, ChainConfig, DexFactoryConfig,
    PoolTypeConfig,
};

// ─────────────────────────────────────────────────────────────
// Top-level settings struct
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub application: ApplicationSettings,
    #[serde(default)]
    pub database: DatabaseSettings,
    #[serde(default)]
    pub redis: RedisSettings,
    #[serde(default)]
    pub nats: NatsSettings,
    #[serde(default)]
    pub auth: AuthSettings,
    #[serde(default)]
    pub kms: KmsSettings,
    #[serde(default)]
    pub session_api: SessionApiSettings,
    #[serde(default)]
    pub api_gateway: ApiGatewaySettings,
    #[serde(default)]
    pub websocket: WebsocketSettings,
    #[serde(default)]
    pub execution: ExecutionSettings,
    #[serde(default)]
    pub pricing: PricingSettings,
    #[serde(default)]
    pub smtp: SmtpSettings,
    #[serde(default)]
    pub chains: HashMap<String, ChainConfig>,
}

// ─────────────────────────────────────────────────────────────
// Section structs
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ApplicationSettings {
    #[serde(default = "default_env")]
    pub environment: String,
}
impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            environment: default_env(),
        }
    }
}
fn default_env() -> String {
    "development".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    #[serde(default = "default_db_url")]
    pub url: String,
    #[serde(default = "default_max_conn")]
    pub max_connections: u32,
}
impl Default for DatabaseSettings {
    fn default() -> Self {
        Self {
            url: default_db_url(),
            max_connections: default_max_conn(),
        }
    }
}
fn default_db_url() -> String {
    "postgresql://liquifier:liquifier@localhost:5432/liquifier".into()
}
fn default_max_conn() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisSettings {
    #[serde(default = "default_redis_url")]
    pub url: String,
}
impl Default for RedisSettings {
    fn default() -> Self {
        Self {
            url: default_redis_url(),
        }
    }
}
fn default_redis_url() -> String {
    "redis://localhost:6379".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct NatsSettings {
    #[serde(default = "default_nats_url")]
    pub url: String,
}
impl Default for NatsSettings {
    fn default() -> Self {
        Self {
            url: default_nats_url(),
        }
    }
}
fn default_nats_url() -> String {
    "nats://localhost:4222".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthSettings {
    #[serde(default)]
    pub jwt_secret: String,
    #[serde(default = "default_access_expiry")]
    pub access_token_expiry_secs: u64,
    #[serde(default = "default_refresh_expiry")]
    pub refresh_token_expiry_secs: u64,
}
impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            access_token_expiry_secs: default_access_expiry(),
            refresh_token_expiry_secs: default_refresh_expiry(),
        }
    }
}
fn default_access_expiry() -> u64 {
    3600
}
fn default_refresh_expiry() -> u64 {
    604800
}

#[derive(Debug, Clone, Deserialize)]
pub struct KmsSettings {
    #[serde(default = "default_kms_port")]
    pub grpc_port: u16,
    #[serde(default = "default_kms_grpc_addr")]
    pub grpc_addr: String,
    #[serde(default)]
    pub master_encryption_key: String,
}
impl Default for KmsSettings {
    fn default() -> Self {
        Self {
            grpc_port: default_kms_port(),
            grpc_addr: default_kms_grpc_addr(),
            master_encryption_key: String::new(),
        }
    }
}
fn default_kms_port() -> u16 {
    50051
}
fn default_kms_grpc_addr() -> String {
    "http://localhost:50051".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionApiSettings {
    #[serde(default = "default_session_port")]
    pub grpc_port: u16,
    #[serde(default = "default_session_grpc_addr")]
    pub grpc_addr: String,
    #[serde(default)]
    pub evm_rpc_url: String,
}
impl Default for SessionApiSettings {
    fn default() -> Self {
        Self {
            grpc_port: default_session_port(),
            grpc_addr: default_session_grpc_addr(),
            evm_rpc_url: String::new(),
        }
    }
}
fn default_session_port() -> u16 {
    50052
}
fn default_session_grpc_addr() -> String {
    "http://localhost:50052".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiGatewaySettings {
    #[serde(default = "default_api_port")]
    pub http_port: u16,
}
impl Default for ApiGatewaySettings {
    fn default() -> Self {
        Self {
            http_port: default_api_port(),
        }
    }
}
fn default_api_port() -> u16 {
    8080
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebsocketSettings {
    #[serde(default = "default_ws_http_port")]
    pub http_port: u16,
    #[serde(default = "default_ws_grpc_port")]
    pub grpc_port: u16,
    #[serde(default = "default_ws_grpc_addr")]
    pub grpc_addr: String,
}
impl Default for WebsocketSettings {
    fn default() -> Self {
        Self {
            http_port: default_ws_http_port(),
            grpc_port: default_ws_grpc_port(),
            grpc_addr: default_ws_grpc_addr(),
        }
    }
}
fn default_ws_http_port() -> u16 {
    8081
}
fn default_ws_grpc_port() -> u16 {
    50053
}
fn default_ws_grpc_addr() -> String {
    "http://localhost:50053".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionSettings {
    #[serde(default = "default_max_impact")]
    pub max_price_impact_bps: u32,
}
impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            max_price_impact_bps: default_max_impact(),
        }
    }
}
fn default_max_impact() -> u32 {
    500
}

#[derive(Debug, Clone, Deserialize)]
pub struct PricingSettings {
    #[serde(default = "default_price_interval")]
    pub update_interval_secs: u64,
}
impl Default for PricingSettings {
    fn default() -> Self {
        Self {
            update_interval_secs: default_price_interval(),
        }
    }
}
fn default_price_interval() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct SmtpSettings {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub from_email: String,
    #[serde(default)]
    pub from_name: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
}
impl Default for SmtpSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_smtp_port(),
            username: String::new(),
            password: String::new(),
            from_email: String::new(),
            from_name: String::new(),
            base_url: default_base_url(),
        }
    }
}
fn default_smtp_port() -> u16 {
    587
}
fn default_base_url() -> String {
    "http://localhost:3000".into()
}

// ─────────────────────────────────────────────────────────────
// Loading logic (modeled after peniwallet-config)
// ─────────────────────────────────────────────────────────────

/// Embedded default config (compiled into the binary)
const DEFAULT_CONFIG: &str = include_str!("../../../config/default.yml");
const PRODUCTION_CONFIG: &str = include_str!("../../../config/production.yml");

static SETTINGS: OnceLock<Settings> = OnceLock::new();

impl Settings {
    /// Load settings with layered sources:
    /// 1. Embedded default.yml
    /// 2. Embedded environment-specific yml (e.g. production.yml)
    /// 3. Optional local config files on disk
    /// 4. Environment variables with APP__ prefix
    pub fn new() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok();

        let app_environment =
            std::env::var("APP_ENVIRONMENT").unwrap_or_else(|_| "development".into());

        // Layer 1: embedded default config
        let mut builder = Config::builder().add_source(config::File::from_str(
            DEFAULT_CONFIG,
            config::FileFormat::Yaml,
        ));

        // Layer 2: embedded environment-specific config
        if app_environment == "production" {
            builder = builder.add_source(config::File::from_str(
                PRODUCTION_CONFIG,
                config::FileFormat::Yaml,
            ));
        }

        // Layer 3: local config files (if present on disk, e.g. during dev)
        let root = Self::find_project_root();
        let config_dir = root.join("config");
        let local_default = config_dir.join("default.yml");
        let local_env = config_dir.join(format!("{}.yml", app_environment));

        if local_default.exists() {
            builder = builder.add_source(File::from(local_default).required(false));
        }
        if local_env.exists() {
            builder = builder.add_source(File::from(local_env).required(false));
        }

        // Layer 4: environment variable overrides
        // e.g. APP__DATABASE__URL=postgres://...
        // e.g. APP__CHAINS__ETHEREUM__RPC_URL=https://...
        builder = builder.add_source(
            Environment::with_prefix("APP")
                .separator("__")
                .ignore_empty(true),
        );

        builder.build()?.try_deserialize()
    }

    /// Initialize the global singleton. Call once at startup.
    pub fn init() -> Result<&'static Settings, ConfigError> {
        let settings = Self::new()?;
        Ok(SETTINGS.get_or_init(|| settings))
    }

    /// Get the global settings. Panics if `init()` hasn't been called.
    pub fn global() -> &'static Settings {
        SETTINGS
            .get()
            .expect("Settings::init() must be called before Settings::global()")
    }

    pub fn is_production(&self) -> bool {
        self.application.environment == "production"
    }

    pub fn is_development(&self) -> bool {
        self.application.environment == "development"
    }

    fn find_project_root() -> PathBuf {
        let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        loop {
            if dir.join("Cargo.lock").exists() || dir.join("config").is_dir() {
                return dir;
            }
            if !dir.pop() {
                return PathBuf::from(".");
            }
        }
    }
}
