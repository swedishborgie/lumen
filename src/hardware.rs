use std::path::PathBuf;

use crate::cli::Args;

/// Scan `/dev/dri/` for the first `renderD*` node. Returns `None` if none is
/// found (triggers the CPU/Pixman renderer path).
fn detect_dri_node() -> Option<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("renderD"))
                .unwrap_or(false)
        })
        .collect();
    nodes.sort();
    if let Some(ref node) = nodes.first() {
        tracing::info!(node = %node.display(), "Auto-detected DRI render node");
    } else {
        tracing::info!("No /dev/dri/renderD* found; using CPU (Pixman) renderer");
    }
    nodes.into_iter().next()
}

/// Determine the effective DRI render node, probing VA-API availability.
///
/// Uses `--dri-node` if provided, otherwise auto-detects.  When a node is
/// found, VA-API is probed before the compositor is created.  If VA-API is
/// unavailable (missing driver, permissions, etc.) the function returns `None`
/// so that both the compositor and the encoder use the CPU path, avoiding a
/// renderer/encoder mismatch.
pub fn detect_and_probe_gpu(args: &Args) -> Option<PathBuf> {
    let dri_node = args.dri_node.clone().or_else(detect_dri_node);
    if let Some(ref node) = dri_node {
        let probe_config = lumen_encode::EncoderConfig {
            render_node: Some(node.clone()),
            ..Default::default()
        };
        if lumen_encode::probe_vaapi(&probe_config) {
            Some(node.clone())
        } else {
            tracing::warn!(
                node = %node.display(),
                "VA-API unavailable on the requested DRI node; \
                 falling back to CPU (Pixman) rendering and software x264 encoder"
            );
            None
        }
    } else {
        None
    }
}
