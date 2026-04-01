use crate::{AppState, auth};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

// Routes that do not require a JWT
const PUBLIC_ROUTES: &[&str] = &[
    "/api/auth/signup",
    "/api/auth/login",
];

pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Allow public routes and the public share link through without auth
    if PUBLIC_ROUTES.contains(&path.as_str()) || path.starts_with("/api/public/") {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let token = match auth_header {
        Some(t) => t.to_string(),
        None => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": "Missing Authorization header" })),
            )
                .into_response();
        }
    };

    match auth::decode_jwt(&token, &state.jwt_secret) {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

use axum::response::IntoResponse;
