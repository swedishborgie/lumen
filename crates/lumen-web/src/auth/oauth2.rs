//! OpenID Connect OAuth2 authorization code flow with PKCE.
//!
//! On the first unauthenticated request the middleware redirects the browser to
//! the configured OIDC provider.  The provider redirects back to
//! `/auth/callback`, where the authorization code is exchanged for an ID token.
//! The `sub` claim is validated against the configured `expected_subject`.  On
//! success a session cookie is set and the user is redirected to `/`.
//!
//! Sessions are stored in an in-memory [`SessionStore`] — an
//! `Arc<RwLock<HashMap>>` — keyed by a random UUID stored in a `lumen_session`
//! cookie.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use anyhow::Result;
use axum::{
    Extension,
    extract::{Query, Request},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use cookie::{Cookie, SameSite};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest::async_http_client,
};
use serde::Deserialize;
use uuid::Uuid;

const COOKIE_NAME: &str = "lumen_session";

/// Per-session data stored in memory.
#[derive(Default, Clone)]
struct SessionData {
    authenticated: bool,
    /// PKCE code verifier secret, present between the redirect and callback.
    pkce_verifier: Option<String>,
    /// Nonce secret, present between the redirect and callback.
    nonce: Option<String>,
    /// CSRF / state parameter, present between the redirect and callback.
    csrf_token: Option<String>,
}

/// In-memory session store: session token → [`SessionData`].
type SessionStore = Arc<RwLock<HashMap<String, SessionData>>>;

/// Shared OIDC client state, initialized at server startup via [`OidcState::discover`].
#[derive(Clone)]
pub struct OidcState {
    client: Arc<CoreClient>,
    expected_subject: String,
    sessions: SessionStore,
}

impl OidcState {
    /// Fetch the OIDC discovery document from
    /// `{issuer_url}/.well-known/openid-configuration` and build the client.
    pub async fn discover(
        issuer_url: String,
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        expected_subject: String,
    ) -> Result<Self> {
        let provider_metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer_url)?,
            async_http_client,
        )
        .await
        .map_err(|e| anyhow::anyhow!("OIDC discovery failed: {e}"))?;

        let client = CoreClient::from_provider_metadata(
            provider_metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(RedirectUrl::new(redirect_uri)?);

        Ok(Self {
            client: Arc::new(client),
            expected_subject,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}

// ── Session helpers ──────────────────────────────────────────────────────────

/// Extract the session token from the `lumen_session` cookie.
fn session_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    // Parse the Cookie header value using the cookie crate.
    raw.split(';')
        .filter_map(|s| Cookie::parse(s.trim().to_string()).ok())
        .find(|c| c.name() == COOKIE_NAME)
        .map(|c| c.value().to_string())
}

/// Build a `Set-Cookie` response header value for the session token.
fn set_cookie_header(token: &str) -> String {
    let mut c = Cookie::new(COOKIE_NAME, token.to_string());
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.to_string()
}

/// Build a `Set-Cookie` header that expires (clears) the session cookie.
fn clear_cookie_header() -> String {
    let mut c = Cookie::new(COOKIE_NAME, "");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_max_age(cookie::time::Duration::ZERO);
    c.to_string()
}

/// Redirect to `/` while clearing the session cookie so the auth middleware
/// starts a fresh OIDC flow on the next request.
fn restart_flow(reason: &str) -> Response {
    tracing::info!("OAuth2 session invalid ({reason}), clearing cookie and restarting flow");
    let mut response = Redirect::to("/").into_response();
    if let Ok(val) = clear_cookie_header().parse() {
        response.headers_mut().insert(header::SET_COOKIE, val);
    }
    response
}

// ── Middleware ────────────────────────────────────────────────────────────────

/// Axum middleware that enforces OAuth2/OIDC authentication.
///
/// Requests to `/auth/callback` are passed through unconditionally so the OIDC
/// provider can complete the authorization code exchange.  All other
/// unauthenticated requests are redirected to the OIDC provider.
pub async fn auth_middleware(
    Extension(oidc): Extension<Arc<OidcState>>,
    request: Request,
    next: Next,
) -> Response {
    // Allow the callback through unconditionally.
    if request.uri().path() == "/auth/callback" {
        return next.run(request).await;
    }

    // Check existing session cookie.
    if let Some(token) = session_token(request.headers()) {
        let authenticated = oidc
            .sessions
            .read()
            .ok()
            .and_then(|s| s.get(&token).map(|d| d.authenticated))
            .unwrap_or(false);
        if authenticated {
            return next.run(request).await;
        }
    }

    // Not authenticated: begin authorization code flow.
    redirect_to_provider(oidc)
}

fn redirect_to_provider(oidc: Arc<OidcState>) -> Response {
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_token, nonce) = oidc
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("openid".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    let token = Uuid::new_v4().to_string();
    if let Ok(mut sessions) = oidc.sessions.write() {
        sessions.insert(
            token.clone(),
            SessionData {
                authenticated: false,
                pkce_verifier: Some(pkce_verifier.secret().clone()),
                nonce: Some(nonce.secret().clone()),
                csrf_token: Some(csrf_token.secret().clone()),
            },
        );
    }

    let mut response = Redirect::temporary(auth_url.as_str()).into_response();
    if let Ok(cookie_val) = set_cookie_header(&token).parse() {
        response.headers_mut().insert(header::SET_COOKIE, cookie_val);
    }
    response
}

// ── Callback handler ─────────────────────────────────────────────────────────

/// Query parameters sent by the OIDC provider to `/auth/callback`.
#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    code: String,
    state: String,
}

/// Handles `GET /auth/callback`.
///
/// Exchanges the authorization code for tokens, validates the ID token, checks
/// the `sub` claim, and marks the session as authenticated.
pub async fn callback_handler(
    Extension(oidc): Extension<Arc<OidcState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    let token = match session_token(&headers) {
        Some(t) => t,
        None => return restart_flow("no session cookie"),
    };

    let (pkce_secret, nonce_secret, csrf_secret) = {
        let sessions = match oidc.sessions.read() {
            Ok(s) => s,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        match sessions.get(&token) {
            Some(d) => (
                d.pkce_verifier.clone(),
                d.nonce.clone(),
                d.csrf_token.clone(),
            ),
            None => return restart_flow("unknown session token"),
        }
    };

    let (Some(pkce_secret), Some(nonce_secret), Some(csrf_secret)) =
        (pkce_secret, nonce_secret, csrf_secret)
    else {
        return restart_flow("session missing PKCE/nonce/CSRF state");
    };

    // Verify the state / CSRF parameter.
    if params.state != csrf_secret {
        return restart_flow("CSRF state mismatch");
    }

    // Exchange authorization code for tokens.
    let token_response = oidc
        .client
        .exchange_code(AuthorizationCode::new(params.code))
        .set_pkce_verifier(PkceCodeVerifier::new(pkce_secret))
        .request_async(async_http_client)
        .await;

    let token_response = match token_response {
        Ok(t) => t,
        Err(e) => {
            // Log with Debug to surface the provider's error_code + error_description,
            // which Display ("Server returned error response") omits.
            tracing::warn!("OAuth2 token exchange failed: {e:?}");
            return (StatusCode::UNAUTHORIZED, "Token exchange failed").into_response();
        }
    };

    // Validate the ID token.
    let id_token = match token_response.id_token() {
        Some(t) => t,
        None => {
            tracing::warn!("OAuth2 callback: provider returned no ID token");
            return (StatusCode::UNAUTHORIZED, "No ID token in response").into_response();
        }
    };

    let nonce = Nonce::new(nonce_secret);
    let claims = match id_token.claims(&oidc.client.id_token_verifier(), &nonce) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("OAuth2 ID token validation failed: {e:?}");
            return (StatusCode::UNAUTHORIZED, "Invalid ID token").into_response();
        }
    };

    // Check the subject claim against the configured expected value.
    let subject = claims.subject().as_str();
    if subject != oidc.expected_subject {
        tracing::warn!(
            subject,
            expected = %oidc.expected_subject,
            "OAuth2 login denied: subject mismatch"
        );
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    // Mark the session as authenticated.
    if let Ok(mut sessions) = oidc.sessions.write() {
        if let Some(data) = sessions.get_mut(&token) {
            data.authenticated = true;
            data.pkce_verifier = None;
            data.nonce = None;
            data.csrf_token = None;
        }
    }

    tracing::info!(subject, "OAuth2 login successful");
    Redirect::to("/").into_response()
}

