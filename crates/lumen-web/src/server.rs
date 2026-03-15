use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{middleware, routing::get, Router};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};

use crate::auth::{basic, oauth2};
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
        let static_dir = ServeDir::new(&self.config.static_dir);

        match &self.config.auth {
            AuthConfig::None => Ok(signaling_router
                .fallback_service(static_dir)
                .layer(CorsLayer::permissive())
                .layer(TraceLayer::new_for_http())),

            AuthConfig::Basic => Ok(signaling_router
                .fallback_service(static_dir)
                .layer(middleware::from_fn(basic::auth_middleware))
                .layer(CorsLayer::permissive())
                .layer(TraceLayer::new_for_http())),

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
                    .merge(signaling_router)
                    .fallback_service(static_dir)
                    .layer(middleware::from_fn(oauth2::auth_middleware))
                    .layer(axum::Extension(oidc_arc))
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
        AuthConfig::OAuth2 { .. } => "oauth2",
    }
}
