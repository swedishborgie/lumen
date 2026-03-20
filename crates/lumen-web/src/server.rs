use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use axum::{middleware, routing::get, Router};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::assets::{index_handler, static_file_handler};
use crate::auth::{basic, bearer, oauth2};
use crate::signaling::{SignalingState, config_handler, ws_handler};
use crate::types::{AuthConfig, WebServerConfig};

pub struct WebServer {
    config: WebServerConfig,
}

impl WebServer {
    pub fn new(config: WebServerConfig) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        let state = SignalingState {
            sessions: self.config.session_manager.clone(),
            input_tx: self.config.input_tx.clone(),
            keyframe_flag: self.config.keyframe_flag.clone(),
            last_cursor_json: self.config.last_cursor_json.clone(),
            last_clipboard_json: self.config.last_clipboard_json.clone(),
            resize_tx: self.config.resize_tx.clone(),
            ice_servers: self.config.ice_servers.clone(),
        };

        let signaling_router = Router::new()
            .route("/ws/signal", get(ws_handler))
            .route("/api/config", get(config_handler))
            .with_state(state);

        let app = self.build_app(signaling_router).await?;

        match (self.config.tls_cert, self.config.tls_key) {
            (Some(cert), Some(key)) => {
                let tls_config =
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to load TLS certificate ({}) and key ({})",
                                cert.display(),
                                key.display()
                            )
                        })?;

                tokio::spawn(watch_tls_cert(tls_config.clone(), cert.clone(), key.clone()));

                tracing::info!(
                    addr = %self.config.bind_addr,
                    auth = %auth_mode_name(&self.config.auth),
                    "Web server listening (HTTPS)"
                );

                let handle = axum_server::Handle::new();
                if let Some(shutdown_rx) = self.config.shutdown_signal {
                    let h = handle.clone();
                    tokio::spawn(async move {
                        shutdown_rx.await.ok();
                        h.graceful_shutdown(None);
                    });
                }

                axum_server::bind_rustls(self.config.bind_addr, tls_config)
                    .handle(handle)
                    .serve(app.into_make_service())
                    .await?;
            }
            (None, None) => {
                let listener =
                    tokio::net::TcpListener::bind(self.config.bind_addr).await?;
                tracing::info!(
                    addr = %self.config.bind_addr,
                    auth = %auth_mode_name(&self.config.auth),
                    "Web server listening (HTTP)"
                );
                let serve = axum::serve(listener, app);
                if let Some(shutdown_rx) = self.config.shutdown_signal {
                    serve
                        .with_graceful_shutdown(async move { shutdown_rx.await.ok(); })
                        .await?;
                } else {
                    serve.await?;
                }
            }
            _ => {
                anyhow::bail!(
                    "--tls-cert and --tls-key must be provided together; supply both or neither"
                );
            }
        }

        Ok(())
    }

    async fn build_app(&self, signaling_router: Router) -> Result<Router> {
        // Explicit routes that require authentication. The index page is routed
        // explicitly so that auth protects it while leaving other static assets
        // (service worker, manifest, CSS, JS, images) accessible without
        // credentials — service worker fetches do not carry auth headers.
        let protected = signaling_router
            .route("/", get(index_handler))
            .route("/index.html", get(index_handler));

        match &self.config.auth {
            AuthConfig::None => Ok(protected
                .fallback(static_file_handler)
                .layer(CorsLayer::permissive())
                .layer(TraceLayer::new_for_http())),

            AuthConfig::Basic => Ok(protected
                .route_layer(middleware::from_fn(basic::auth_middleware))
                .fallback(static_file_handler)
                .layer(CorsLayer::permissive())
                .layer(TraceLayer::new_for_http())),

            AuthConfig::Bearer { token } => {
                let token_arc: std::sync::Arc<str> = token.clone().into();
                Ok(protected
                    .route_layer(middleware::from_fn(bearer::auth_middleware))
                    .route_layer(axum::Extension(token_arc))
                    .fallback(static_file_handler)
                    .layer(CorsLayer::permissive())
                    .layer(TraceLayer::new_for_http()))
            }

            AuthConfig::OAuth2 {
                issuer_url,
                client_id,
                client_secret,
                redirect_uri,
                expected_subject,
            } => {
                tracing::info!("Discovering OIDC configuration from {issuer_url}");
                let oidc = oauth2::OidcState::discover(
                    issuer_url.clone(),
                    client_id.clone(),
                    client_secret.clone(),
                    redirect_uri.clone(),
                    expected_subject.clone(),
                )
                .await?;
                let oidc_arc = Arc::new(oidc);

                let app = Router::new()
                    .route("/auth/callback", get(oauth2::callback_handler))
                    .merge(protected)
                    .route_layer(middleware::from_fn(oauth2::auth_middleware))
                    .route_layer(axum::Extension(oidc_arc))
                    .fallback(static_file_handler)
                    .layer(CorsLayer::permissive())
                    .layer(TraceLayer::new_for_http());

                Ok(app)
            }
        }
    }
}

fn auth_mode_name(auth: &AuthConfig) -> &'static str {
    match auth {
        AuthConfig::None => "none",
        AuthConfig::Basic => "basic",
        AuthConfig::Bearer { .. } => "bearer",
        AuthConfig::OAuth2 { .. } => "oauth2",
    }
}

/// Polls the TLS certificate and key files every 30 seconds and reloads them
/// when either file changes. On a reload error (e.g. a cert/key mismatch
/// during a mid-rotation write), the stored fingerprints are left unchanged so
/// the next tick automatically retries.
async fn watch_tls_cert(
    config: axum_server::tls_rustls::RustlsConfig,
    cert: PathBuf,
    key: PathBuf,
) {
    /// Returns `(mtime, size)` for a file, or `None` if metadata cannot be read.
    fn fingerprint(path: &PathBuf) -> Option<(SystemTime, u64)> {
        std::fs::metadata(path)
            .ok()
            .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
    }

    let mut last = (fingerprint(&cert), fingerprint(&key));

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.tick().await; // consume the immediate first tick

    loop {
        interval.tick().await;

        let current = (fingerprint(&cert), fingerprint(&key));
        if current == last {
            continue;
        }

        match config.reload_from_pem_file(&cert, &key).await {
            Ok(()) => {
                tracing::info!(
                    cert = %cert.display(),
                    key  = %key.display(),
                    "TLS certificate reloaded"
                );
                last = current;
            }
            Err(err) => {
                // Don't update `last` — we'll retry on the next tick.  This
                // handles mid-rotation races where the cert and key files are
                // not written atomically.
                tracing::warn!(
                    cert = %cert.display(),
                    key  = %key.display(),
                    %err,
                    "TLS certificate reload failed; will retry"
                );
            }
        }
    }
}
