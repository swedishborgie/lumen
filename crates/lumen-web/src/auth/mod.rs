//! Authentication support for the Lumen web server.
//!
//! Three modes are supported, selected via [`crate::types::AuthConfig`]:
//!
//! - **None** — no authentication (open access, same as before)
//! - **Basic** — HTTP Basic dialog validated against the system PAM
//! - **OAuth2** — OpenID Connect authorization code flow with PKCE

pub mod basic;
pub mod oauth2;
