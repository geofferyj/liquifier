use axum::{
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Authentication failed: {0}")]
    Unauthorized(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Internal server error")]
    Internal(#[from] anyhow::Error),
    #[error("Database error")]
    Database(#[from] sqlx::Error),
    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            Self::Unauthorized(m)  => (StatusCode::UNAUTHORIZED,            m.clone()),
            Self::NotFound(m)      => (StatusCode::NOT_FOUND,               m.clone()),
            Self::BadRequest(m)    => (StatusCode::BAD_REQUEST,             m.clone()),
            Self::Conflict(m)      => (StatusCode::CONFLICT,                m.clone()),
            Self::Internal(_)      => (StatusCode::INTERNAL_SERVER_ERROR,   "Internal server error".into()),
            Self::Database(_)      => (StatusCode::INTERNAL_SERVER_ERROR,   "Database error".into()),
            Self::Grpc(s)          => (StatusCode::INTERNAL_SERVER_ERROR,   s.message().into()),
        };

        tracing::error!("{self}");
        (status, Json(json!({ "error": message }))).into_response()
    }
}
