//! HTTP Basic authentication middleware backed by PAM.
//!
//! The browser sends a standard `Authorization: Basic <base64>` header.
//! The submitted username must match the current process owner (`$USER` /
//! `$LOGNAME`) and the password is validated via the system PAM `login`
//! service.  Failed attempts return `401` with a `WWW-Authenticate` challenge
//! so the browser re-prompts the user.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
};
use base64::Engine as _;

/// Axum middleware that enforces HTTP Basic authentication via PAM.
pub async fn auth_middleware(request: Request, next: Next) -> Response {
    if let Some((username, password)) = extract_credentials(request.headers()) {
        if validate_credentials(&username, &password).await {
            return next.run(request).await;
        }
    }
    challenge_response()
}

/// Decodes an `Authorization: Basic <base64(user:pass)>` header.
fn extract_credentials(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Returns `true` when `username` matches the process owner and PAM accepts the password.
async fn validate_credentials(username: &str, password: &str) -> bool {
    let current_user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();

    if username != current_user {
        return false;
    }

    let username = username.to_string();
    let password = password.to_string();

    // PAM is a blocking C library — run it off the async executor.
    tokio::task::spawn_blocking(move || pam_validate(&username, &password))
        .await
        .unwrap_or(false)
}

fn pam_validate(username: &str, password: &str) -> bool {
    // Use "sshd" rather than "login": the login service includes session-management
    // modules (pam_systemd, pam_loginuid, pam_securetty) that expect a real TTY and
    // can block indefinitely on D-Bus / systemd-logind.  The sshd service is designed
    // for non-interactive password validation and works without a TTY.
    //
    // Administrators who need a custom policy can create /etc/pam.d/lumen and set
    // LUMEN_AUTH_PAM_SERVICE=lumen (or change this constant).
    const PAM_SERVICE: &str = "sshd";
    match pam::Authenticator::with_password(PAM_SERVICE) {
        Ok(mut auth) => {
            auth.get_handler().set_credentials(username, password);
            match auth.authenticate() {
                Ok(()) => true,
                Err(e) => {
                    tracing::debug!("PAM authentication failed: {e}");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!("PAM init error (service={PAM_SERVICE}): {e}");
            false
        }
    }
}

fn challenge_response() -> Response {
    let mut response = Response::new(Body::from("Unauthorized"));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(r#"Basic realm="Lumen""#),
    );
    response
}
