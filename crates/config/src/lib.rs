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

    /// Validate that all enabled chains have real (non-placeholder) metadata.
    /// Returns a list of validation errors. An empty list means all checks pass.
    pub fn validate_enabled_chains(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (chain_name, chain_cfg) in &self.chains {
            if !chain_cfg.enabled {
                continue;
            }
            if chain_cfg.rpc_url.trim().is_empty() {
                errors.push(format!(
                    "Chain '{chain_name}' is enabled but has no rpc_url configured"
                ));
            }
            if chain_cfg.base_tokens.is_empty() {
                errors.push(format!(
                    "Chain '{chain_name}' is enabled but has no base_tokens"
                ));
            }
            for (idx, bt) in chain_cfg.base_tokens.iter().enumerate() {
                if is_placeholder_address(&bt.address) {
                    errors.push(format!(
                        "Chain '{chain_name}' base_token[{idx}] has a placeholder address: {}",
                        bt.address
                    ));
                }
                if bt.symbol.eq_ignore_ascii_case("DUMMY") {
                    errors.push(format!(
                        "Chain '{chain_name}' base_token[{idx}] has DUMMY symbol"
                    ));
                }
                if !bt.chainlink_oracle.is_empty() && is_placeholder_address(&bt.chainlink_oracle) {
                    errors.push(format!(
                        "Chain '{chain_name}' base_token[{idx}] has a placeholder oracle address: {}",
                        bt.chainlink_oracle
                    ));
                }
            }
            for (idx, dex) in chain_cfg.dex_factories.iter().enumerate() {
                if is_placeholder_address(&dex.factory_address) {
                    errors.push(format!(
                        "Chain '{chain_name}' dex_factories[{idx}] ({}) has a placeholder factory address",
                        dex.name
                    ));
                }
                if is_placeholder_address(&dex.router_address) {
                    errors.push(format!(
                        "Chain '{chain_name}' dex_factories[{idx}] ({}) has a placeholder router address",
                        dex.name
                    ));
                }
            }
        }
        errors
    }

    /// Validate and panic on startup if enabled chains have placeholder metadata.
    /// In development mode, logs warnings instead of panicking.
    pub fn validate_or_warn(&self) {
        let errors = self.validate_enabled_chains();
        if errors.is_empty() {
            return;
        }
        if self.is_development() {
            for err in &errors {
                eprintln!("[WARN] Config validation: {err}");
            }
        } else {
            for err in &errors {
                eprintln!("[ERROR] Config validation: {err}");
            }
            panic!(
                "Configuration validation failed with {} error(s) — fix config before deploying",
                errors.len()
            );
        }
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

/// Check whether an address is a known placeholder (zero-padded sequential
/// addresses like 0x0000…0001, 0x0000…0002, etc., or the zero address).
fn is_placeholder_address(address: &str) -> bool {
    let addr = address.trim().to_lowercase();
    if addr.is_empty() {
        return true;
    }
    let hex = addr.strip_prefix("0x").unwrap_or(&addr);
    if hex.len() != 40 {
        return false;
    }
    // Count leading zeros — genuine addresses rarely have 30+ leading zero hex digits
    let leading_zeros = hex.chars().take_while(|c| *c == '0').count();
    leading_zeros >= 30
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_placeholder_address ──────────────────────────────
    #[test]
    fn test_placeholder_detection() {
        assert!(is_placeholder_address(
            "0x0000000000000000000000000000000000000001"
        ));
        assert!(is_placeholder_address(
            "0x0000000000000000000000000000000000000004"
        ));
        assert!(is_placeholder_address(
            "0x0000000000000000000000000000000000000000"
        ));
        // Real BSC PancakeSwap factory
        assert!(!is_placeholder_address(
            "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73"
        ));
        // Real WBNB
        assert!(!is_placeholder_address(
            "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c"
        ));
        assert!(is_placeholder_address(""));
    }

    #[test]
    fn test_placeholder_short_hex() {
        // Less than 40 hex chars → not a placeholder
        assert!(!is_placeholder_address("0x000000000000000001"));
    }

    #[test]
    fn test_placeholder_with_whitespace() {
        assert!(is_placeholder_address(
            "  0x0000000000000000000000000000000000000001  "
        ));
    }

    #[test]
    fn test_placeholder_no_0x_prefix() {
        // No 0x prefix but 40 hex chars with many leading zeros
        assert!(is_placeholder_address(
            "0000000000000000000000000000000000000001"
        ));
    }

    #[test]
    fn test_placeholder_29_leading_zeros_not_placeholder() {
        // 29 leading zeros followed by 11 non-zero chars → not a placeholder (need >=30)
        assert!(!is_placeholder_address(
            "0x00000000000000000000000000000123456789ab"
        ));
    }

    // ── Default values ──────────────────────────────────────
    #[test]
    fn test_default_application_settings() {
        let s = ApplicationSettings::default();
        assert_eq!(s.environment, "development");
    }

    #[test]
    fn test_default_database_settings() {
        let s = DatabaseSettings::default();
        assert!(s.url.contains("liquifier"));
        assert_eq!(s.max_connections, 10);
    }

    #[test]
    fn test_default_redis_settings() {
        let s = RedisSettings::default();
        assert!(s.url.starts_with("redis://"));
    }

    #[test]
    fn test_default_nats_settings() {
        let s = NatsSettings::default();
        assert!(s.url.starts_with("nats://"));
    }

    #[test]
    fn test_default_auth_settings() {
        let s = AuthSettings::default();
        assert!(s.jwt_secret.is_empty());
        assert_eq!(s.access_token_expiry_secs, 3600);
        assert_eq!(s.refresh_token_expiry_secs, 604800);
    }

    #[test]
    fn test_default_kms_settings() {
        let s = KmsSettings::default();
        assert_eq!(s.grpc_port, 50051);
        assert!(s.master_encryption_key.is_empty());
    }

    #[test]
    fn test_default_session_api_settings() {
        let s = SessionApiSettings::default();
        assert_eq!(s.grpc_port, 50052);
    }

    #[test]
    fn test_default_api_gateway_settings() {
        let s = ApiGatewaySettings::default();
        assert_eq!(s.http_port, 8080);
    }

    #[test]
    fn test_default_websocket_settings() {
        let s = WebsocketSettings::default();
        assert_eq!(s.http_port, 8081);
        assert_eq!(s.grpc_port, 50053);
    }

    #[test]
    fn test_default_execution_settings() {
        let s = ExecutionSettings::default();
        assert_eq!(s.max_price_impact_bps, 500);
    }

    #[test]
    fn test_default_pricing_settings() {
        let s = PricingSettings::default();
        assert_eq!(s.update_interval_secs, 30);
    }

    #[test]
    fn test_default_smtp_settings() {
        let s = SmtpSettings::default();
        assert!(s.host.is_empty());
        assert_eq!(s.port, 587);
        assert!(s.base_url.contains("localhost"));
    }

    // ── is_production / is_development ──────────────────────
    #[test]
    fn test_is_production() {
        let mut settings = Settings::new().unwrap();
        settings.application.environment = "production".to_string();
        assert!(settings.is_production());
        assert!(!settings.is_development());
    }

    #[test]
    fn test_is_development() {
        let mut settings = Settings::new().unwrap();
        settings.application.environment = "development".to_string();
        assert!(settings.is_development());
        assert!(!settings.is_production());
    }

    // ── validate_enabled_chains ─────────────────────────────
    #[test]
    fn test_validate_no_chains_no_errors() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        assert!(settings.validate_enabled_chains().is_empty());
    }

    #[test]
    fn test_validate_disabled_chain_skipped() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: false,
                chain_id: 999,
                rpc_url: String::new(),
                ws_url: String::new(),
                base_tokens: vec![],
                dex_factories: vec![],
            },
        );
        assert!(settings.validate_enabled_chains().is_empty());
    }

    #[test]
    fn test_validate_enabled_chain_missing_rpc() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 999,
                rpc_url: String::new(),
                ws_url: String::new(),
                base_tokens: vec![BaseToken {
                    address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
                    symbol: "WETH".to_string(),
                    chainlink_oracle: String::new(),
                }],
                dex_factories: vec![],
            },
        );
        let errors = settings.validate_enabled_chains();
        assert!(errors.iter().any(|e| e.contains("rpc_url")));
    }

    #[test]
    fn test_validate_enabled_chain_placeholder_base_token() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 999,
                rpc_url: "https://rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![BaseToken {
                    address: "0x0000000000000000000000000000000000000001".to_string(),
                    symbol: "WETH".to_string(),
                    chainlink_oracle: String::new(),
                }],
                dex_factories: vec![],
            },
        );
        let errors = settings.validate_enabled_chains();
        assert!(errors.iter().any(|e| e.contains("placeholder address")));
    }

    #[test]
    fn test_validate_enabled_chain_dummy_symbol() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 999,
                rpc_url: "https://rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![BaseToken {
                    address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
                    symbol: "DUMMY".to_string(),
                    chainlink_oracle: String::new(),
                }],
                dex_factories: vec![],
            },
        );
        let errors = settings.validate_enabled_chains();
        assert!(errors.iter().any(|e| e.contains("DUMMY")));
    }

    #[test]
    fn test_validate_placeholder_factory_address() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 999,
                rpc_url: "https://rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![BaseToken {
                    address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
                    symbol: "WETH".to_string(),
                    chainlink_oracle: String::new(),
                }],
                dex_factories: vec![DexFactoryConfig {
                    name: "uniswap".to_string(),
                    factory_address: "0x0000000000000000000000000000000000000001".to_string(),
                    router_address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
                    pool_type: PoolTypeConfig::V2,
                    fee_tiers: vec![],
                }],
            },
        );
        let errors = settings.validate_enabled_chains();
        assert!(errors.iter().any(|e| e.contains("placeholder factory")));
    }

    #[test]
    fn test_validate_placeholder_oracle() {
        let mut settings = Settings::new().unwrap();
        settings.chains.clear();
        settings.chains.insert(
            "test".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 999,
                rpc_url: "https://rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![BaseToken {
                    address: "0xcA143Ce32Fe78f1f7019d7d551a6402fC5350c73".to_string(),
                    symbol: "WETH".to_string(),
                    chainlink_oracle: "0x0000000000000000000000000000000000000002".to_string(),
                }],
                dex_factories: vec![],
            },
        );
        let errors = settings.validate_enabled_chains();
        assert!(errors.iter().any(|e| e.contains("placeholder oracle")));
    }

    // ── Settings::new ───────────────────────────────────────
    #[test]
    fn test_settings_new_loads_defaults() {
        let settings = Settings::new().unwrap();
        assert!(!settings.application.environment.is_empty());
        assert!(!settings.database.url.is_empty());
    }
}
