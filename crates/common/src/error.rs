use thiserror::Error;

#[derive(Error, Debug)]
pub enum LiquifierError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("Web3 error: {0}")]
    Web3(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_database() {
        let err = LiquifierError::Database("connection failed".into());
        assert_eq!(err.to_string(), "Database error: connection failed");
    }

    #[test]
    fn test_error_display_auth() {
        let err = LiquifierError::Auth("invalid token".into());
        assert_eq!(err.to_string(), "Authentication failed: invalid token");
    }

    #[test]
    fn test_error_display_not_found() {
        let err = LiquifierError::NotFound("session xyz".into());
        assert_eq!(err.to_string(), "Not found: session xyz");
    }

    #[test]
    fn test_error_display_validation() {
        let err = LiquifierError::Validation("missing field".into());
        assert_eq!(err.to_string(), "Validation error: missing field");
    }

    #[test]
    fn test_error_display_crypto() {
        let err = LiquifierError::Crypto("key mismatch".into());
        assert_eq!(err.to_string(), "Crypto error: key mismatch");
    }

    #[test]
    fn test_error_display_web3() {
        let err = LiquifierError::Web3("rpc timeout".into());
        assert_eq!(err.to_string(), "Web3 error: rpc timeout");
    }

    #[test]
    fn test_error_display_internal() {
        let err = LiquifierError::Internal("panic".into());
        assert_eq!(err.to_string(), "Internal error: panic");
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LiquifierError>();
    }

    #[test]
    fn test_error_debug_format() {
        let err = LiquifierError::Database("test".into());
        let debug = format!("{:?}", err);
        assert!(debug.contains("Database"));
    }
}
