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
