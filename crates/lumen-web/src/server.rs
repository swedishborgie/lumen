use anyhow::Result;
use axum::{Router, routing::get};
use tower_http::{cors::CorsLayer, services::ServeDir, trace::TraceLayer};

use crate::signaling::{ws_handler, SignalingState};
use crate::types::WebServerConfig;

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
        };

        let app = Router::new()
            .route("/ws/signal", get(ws_handler))
            .fallback_service(ServeDir::new(&self.config.static_dir))
            .layer(CorsLayer::permissive())
            .layer(TraceLayer::new_for_http())
            .with_state(state);

        let listener = tokio::net::TcpListener::bind(self.config.bind_addr).await?;
        tracing::info!("Web server listening on {}", self.config.bind_addr);
        axum::serve(listener, app).await?;
        Ok(())
    }
}
