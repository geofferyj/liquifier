use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::{auth, SharedState};

/// Extension struct injected into request extensions after auth
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub role: String,
}

/// Middleware that validates JWT from the Authorization header.
pub async fn require_auth(
    State(state): State<SharedState>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token_data =
        auth::validate_token(token, &state.jwt_secret).map_err(|_| StatusCode::UNAUTHORIZED)?;

    if token_data.claims.token_type != "access" {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id: Uuid = token_data
        .claims
        .sub
        .parse()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    request.extensions_mut().insert(AuthUser {
        user_id,
        role: token_data.claims.role,
    });

    Ok(next.run(request).await)
}
