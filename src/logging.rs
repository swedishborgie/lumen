use std::sync::Mutex;

use anyhow::{Context as _, Result};

use crate::cli::{Args, LogOutput};

/// Initialize the tracing subscriber based on the configured log output.
///
/// If `RUST_LOG` is set it is used as-is; otherwise sensible per-crate
/// `info` defaults are applied.
pub fn init(args: &Args) -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("")
                .add_directive("lumen=info".parse().unwrap())
                .add_directive("lumen_compositor=info".parse().unwrap())
                .add_directive("lumen_audio=info".parse().unwrap())
                .add_directive("lumen_encode=info".parse().unwrap())
                .add_directive("lumen_webrtc=info".parse().unwrap())
                .add_directive("lumen_web=info".parse().unwrap())
        });
    match &args.log_output {
        LogOutput::Stderr => {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .init();
        }
        LogOutput::Journald => {
            use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
            match tracing_journald::layer() {
                Ok(journald_layer) => {
                    let journald_layer = match &args.syslog_identifier {
                        Some(id) => journald_layer.with_syslog_identifier(id.clone()),
                        None => journald_layer,
                    };
                    tracing_subscriber::registry()
                        .with(env_filter)
                        .with(journald_layer)
                        .init();
                }
                Err(e) => {
                    tracing_subscriber::fmt()
                        .with_env_filter(env_filter)
                        .init();
                    tracing::warn!("journald unavailable ({e}), falling back to stderr");
                }
            }
        }
        LogOutput::File(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open log file: {}", path.display()))?;
            tracing_subscriber::fmt()
                .with_writer(Mutex::new(file))
                .with_env_filter(env_filter)
                .init();
        }
    }
    Ok(())
}
