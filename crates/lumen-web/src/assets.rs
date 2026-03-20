use axum::{
    body::Body,
    http::{Response, StatusCode, Uri, header},
    response::IntoResponse,
};
use rust_embed::RustEmbed;

/// Embedded static web assets.
///
/// In release builds the `web/` directory is baked into the binary at
/// compile time. In debug builds rust-embed falls back to reading files
/// from disk, so edits to JS/CSS take effect without recompiling.
#[derive(RustEmbed)]
#[folder = "../../web"]
struct Assets;

/// Serves `index.html` from the embedded assets.
pub async fn index_handler() -> impl IntoResponse {
    serve_asset("index.html")
}

/// Fallback handler — serves any other static asset from the embedded
/// assets, stripping the leading `/` from the URI path.
pub async fn static_file_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    serve_asset(path)
}

fn serve_asset(path: &str) -> Response<Body> {
    match Assets::get(path) {
        Some(content) => {
            let mime = content.metadata.mimetype().to_owned();
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .body(Body::from(content.data))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
