//! Bearer token (preshared key) authentication middleware.
//!
//! Every request must include an `Authorization: Bearer <token>` header whose
//! value matches the configured secret.  The comparison is performed in
//! constant time to prevent timing-based token guessing.
//!
//! Intended for use behind a reverse proxy: the proxy injects the header on
//! behalf of clients, preventing direct unauthenticated access to Lumen.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq as _;

/// Axum middleware that enforces bearer token authentication.
///
/// The expected token is passed via [`axum::Extension`] as an
/// `Arc<str>` so it can be shared cheaply across requests.
pub async fn auth_middleware(
    axum::Extension(expected): axum::Extension<std::sync::Arc<str>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(provided) = extract_bearer(request.headers()) {
        if bool::from(provided.as_bytes().ct_eq(expected.as_bytes())) {
            return next.run(request).await;
        }
    }
    challenge_response()
}

/// Extracts the token from `Authorization: Bearer <token>`.
fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    Some(value.strip_prefix("Bearer ")?.to_string())
}

fn challenge_response() -> Response {
    let mut response = Response::new(Body::from("Unauthorized"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(r#"Bearer realm="Lumen""#),
    );
    response
}
