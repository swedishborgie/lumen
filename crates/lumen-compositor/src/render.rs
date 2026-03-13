//! Frame rendering and capture.
//!
//! Each frame: collect Wayland surface elements, render into an offscreen target
//! (DMA-BUF via GlesRenderer, or Vec<u8> via PixmanRenderer), then emit a
//! CapturedFrame for the encoder.

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        gles::GlesRenderer,
        pixman::PixmanRenderer,
        Bind,
    },
    desktop::utils::send_frames_surface_tree,
    output::Output,
    wayland::seat::WaylandFocus,
};
use bytes::Bytes;
use std::time::Duration;

use crate::state::AppState;
use crate::types::CapturedFrame;

/// Render one frame and emit a `CapturedFrame` if successful.
/// Called on every timer tick inside the calloop event loop.
pub fn render_and_capture(state: &mut AppState, damage_tracker: &mut OutputDamageTracker) {
    let now_ms = state.clock.now().as_millis() as u64;
    let width = state.width.cast_unsigned();
    let height = state.height.cast_unsigned();

    let output = match state.outputs.first().cloned() {
        Some(o) => o,
        None => return,
    };

    if state.use_gpu {
        render_gles(state, damage_tracker, &output, now_ms, width, height);
    } else {
        render_pixman(state, damage_tracker, &output, now_ms, width, height);
    }

    // Send wl_surface.frame callbacks so clients know to render their next frame.
    let time = Duration::from_millis(now_ms);
    for window in state.space.elements().cloned().collect::<Vec<_>>() {
        if let Some(surface) = window.wl_surface() {
            send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }

    state.frame_counter = state.frame_counter.wrapping_add(1);
}

// ---------------------------------------------------------------------------
// GPU path — GlesRenderer + DMA-BUF offscreen target
// ---------------------------------------------------------------------------

fn render_gles(
    state: &mut AppState,
    damage_tracker: &mut OutputDamageTracker,
    output: &Output,
    now_ms: u64,
    width: u32,
    height: u32,
) {
    // Take the renderer out of state so we can also borrow state.space simultaneously.
    let mut renderer = match state.gles_renderer.take() {
        Some(r) => r,
        None => return,
    };

    let elements = match collect_elements_gles(&mut renderer, &state.space, output) {
        Some(e) => e,
        None => { state.gles_renderer = Some(renderer); return; }
    };

    let (_, ref mut dmabuf) = match state.offscreen_buffer.as_mut() {
        Some(b) => b,
        None => { state.gles_renderer = Some(renderer); return; }
    };

    let dmabuf_clone = dmabuf.clone();

    let mut target = match renderer.bind(dmabuf) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("GlesRenderer bind error: {:?}", e);
            state.gles_renderer = Some(renderer);
            return;
        }
    };

    match damage_tracker.render_output(&mut renderer, &mut target, 1, &elements, [0.05, 0.05, 0.05, 1.0]) {
        Ok(_) => {
            drop(target);
            state.gles_renderer = Some(renderer);
            let _ = state.frame_tx.send(CapturedFrame {
                rgba_buffer: None,
                dmabuf: Some(dmabuf_clone),
                width,
                height,
                pts_ms: now_ms,
            });
            state.encoded_frame_count += 1;
        }
        Err(e) => {
            tracing::warn!("GlesRenderer render_output error: {:?}", e);
            drop(target);
            state.gles_renderer = Some(renderer);
        }
    }
}

fn collect_elements_gles(
    renderer: &mut GlesRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    output: &Output,
) -> Option<Vec<smithay::desktop::space::SpaceRenderElements<
    GlesRenderer,
    smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement<GlesRenderer>,
>>> {
    space.render_elements_for_output(renderer, output, 1.0)
        .map_err(|e| tracing::warn!("render_elements_for_output (gles) error: {:?}", e))
        .ok()
}

// ---------------------------------------------------------------------------
// CPU path — PixmanRenderer + Vec<u8> frame buffer
// ---------------------------------------------------------------------------

fn render_pixman(
    state: &mut AppState,
    damage_tracker: &mut OutputDamageTracker,
    output: &Output,
    now_ms: u64,
    width: u32,
    height: u32,
) {
    let mut renderer = match state.pixman_renderer.take() {
        Some(r) => r,
        None => return,
    };

    let elements = match collect_elements_pixman(&mut renderer, &state.space, output) {
        Some(e) => e,
        None => { state.pixman_renderer = Some(renderer); return; }
    };

    // Create a Pixman image backed directly by our frame_buffer Vec.
    // SAFETY: frame_buffer is not moved/dropped while pixman_img is alive.
    //         We drop pixman_img before reading frame_buffer below.
    let frame_buffer_ptr = state.frame_buffer.as_mut_ptr() as *mut u32;
    let mut pixman_img = unsafe {
        match smithay::reexports::pixman::Image::from_raw_mut(
            smithay::reexports::pixman::FormatCode::A8R8G8B8,
            width as usize,
            height as usize,
            frame_buffer_ptr,
            (width * 4) as usize,
            false,
        ) {
            Ok(img) => img,
            Err(_) => {
                tracing::warn!("Failed to create pixman image from frame buffer");
                state.pixman_renderer = Some(renderer);
                return;
            }
        }
    };

    let mut target = match renderer.bind(&mut pixman_img) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("PixmanRenderer bind error: {:?}", e);
            state.pixman_renderer = Some(renderer);
            return;
        }
    };

    let render_result = damage_tracker.render_output(
        &mut renderer,
        &mut target,
        1,
        &elements,
        [0.05, 0.05, 0.05, 1.0],
    );

    // Drop target and pixman_img BEFORE reading frame_buffer.
    drop(target);
    drop(pixman_img);
    state.pixman_renderer = Some(renderer);

    match render_result {
        Ok(_) => {
            let rgba = Bytes::copy_from_slice(&state.frame_buffer);
            let _ = state.frame_tx.send(CapturedFrame {
                rgba_buffer: Some(rgba),
                dmabuf: None,
                width,
                height,
                pts_ms: now_ms,
            });
            state.encoded_frame_count += 1;
        }
        Err(e) => {
            tracing::warn!("PixmanRenderer render_output error: {:?}", e);
        }
    }
}

fn collect_elements_pixman(
    renderer: &mut PixmanRenderer,
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    output: &Output,
) -> Option<Vec<smithay::desktop::space::SpaceRenderElements<
    PixmanRenderer,
    smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement<PixmanRenderer>,
>>> {
    space.render_elements_for_output(renderer, output, 1.0)
        .map_err(|e| tracing::warn!("render_elements_for_output (pixman) error: {:?}", e))
        .ok()
}
