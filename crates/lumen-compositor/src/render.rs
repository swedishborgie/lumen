//! Frame rendering and capture.
//!
//! Each frame: collect Wayland surface elements, render into an offscreen target
//! (DMA-BUF via GlesRenderer, or Vec<u8> via PixmanRenderer), then emit a
//! CapturedFrame for the encoder.

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        element::RenderElementStates,
        gles::GlesRenderer,
        pixman::PixmanRenderer,
        Bind,
    },
    desktop::utils::{
        send_frames_surface_tree,
        surface_presentation_feedback_flags_from_states,
        OutputPresentationFeedback,
    },
    output::Output,
    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    utils::{Monotonic, Time},
    wayland::{
        presentation::Refresh,
        seat::WaylandFocus,
    },
};
use bytes::Bytes;
use std::time::{Duration, Instant};

use crate::state::AppState;
use crate::types::CapturedFrame;

/// Render one frame and emit a `CapturedFrame` if successful.
/// Called on every timer tick inside the calloop event loop.
pub fn render_and_capture(state: &mut AppState, damage_tracker: &mut OutputDamageTracker) {
    let now_ms = state.clock.now().as_millis() as u64;
    let captured_at = Instant::now();
    let width = state.width.cast_unsigned();
    let height = state.height.cast_unsigned();

    let output = match state.outputs.first().cloned() {
        Some(o) => o,
        None => return,
    };

    let window_count = state.space.elements().count();
    if state.frame_counter % 150 == 0 {
        tracing::debug!(window_count, "Rendering frame");
    }

    let render_states = if state.use_gpu {
        render_gles(state, damage_tracker, &output, now_ms, captured_at, width, height)
    } else {
        render_pixman(state, damage_tracker, &output, now_ms, captured_at, width, height)
    };

    let time = Duration::from_millis(now_ms);

    // Send wl_surface.frame callbacks so clients know to render their next frame.
    for window in state.space.elements().cloned().collect::<Vec<_>>() {
        if let Some(surface) = window.wl_surface() {
            send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }

    // Resolve wp_presentation feedback for all surfaces.  Clients like kwin use
    // the `presented` event (not just wl_surface.frame) to pace their repaint
    // loop.  Without this, kwin stalls after its first frame — it attaches a
    // wp_presentation_feedback to its surface commit and waits indefinitely for
    // `presented` or `discarded` before scheduling the next repaint.
    if let Some(states) = render_states {
        let mut feedback = OutputPresentationFeedback::new(&output);
        let output_ref = output.clone();
        for window in state.space.elements().cloned().collect::<Vec<_>>() {
            if state.space.outputs_for_element(&window).contains(&output) {
                window.take_presentation_feedback(
                    &mut feedback,
                    // lumen has one output; all surfaces are on it.
                    |_, _| Some(output_ref.clone()),
                    |surface, _| surface_presentation_feedback_flags_from_states(surface, None, &states),
                );
            }
        }
        let refresh = output
            .current_mode()
            .map(|m| Refresh::fixed(Duration::from_secs_f64(1_000f64 / m.refresh as f64)))
            .unwrap_or(Refresh::Unknown);
        feedback.presented::<_, Monotonic>(
            Time::from(time),
            refresh,
            0,
            wp_presentation_feedback::Kind::empty(),
        );
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
    captured_at: Instant,
    width: u32,
    height: u32,
) -> Option<RenderElementStates> {
    // Take the renderer out of state so we can also borrow state.space simultaneously.
    let mut renderer = match state.gles_renderer.take() {
        Some(r) => r,
        None => return None,
    };

    let elements = match collect_elements_gles(&mut renderer, &state.space, output) {
        Some(e) => e,
        None => { state.gles_renderer = Some(renderer); return None; }
    };

    if state.offscreen_buffers.is_empty() {
        state.gles_renderer = Some(renderer);
        return None;
    }

    // Advance to the next slot in the ring before binding.
    // This means we never render into the buffer the encoder most recently
    // received, eliminating the GPU fence stall on discrete GPUs.
    state.offscreen_index = (state.offscreen_index + 1) % state.offscreen_buffers.len();
    let (_, ref mut dmabuf, drm_modifier) = state.offscreen_buffers[state.offscreen_index];

    let dmabuf_clone = dmabuf.clone();

    // Instrument bind time: on a discrete GPU, DRM implicit fences may delay
    // this call if the encoder still holds the buffer.  With the ring, this
    // should be extremely rare (buffer is only reused after OFFSCREEN_RING_SIZE
    // frames), but the warning helps confirm if it does occur.
    let bind_t0 = Instant::now();
    let mut target = match renderer.bind(dmabuf) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("GlesRenderer bind error: {:?}", e);
            state.gles_renderer = Some(renderer);
            return None;
        }
    };
    let bind_elapsed = bind_t0.elapsed();
    if bind_elapsed > Duration::from_millis(5) {
        tracing::warn!(
            ms = bind_elapsed.as_millis(),
            slot = state.offscreen_index,
            "GlesRenderer bind stalled waiting for GPU fence — possible encoder backpressure"
        );
    }

    // age=0: always do a full repaint.  The VA-API encoder processes the entire
    // NV12 frame regardless of damage, so partial repaints offer no benefit on
    // the GPU path and the extra bookkeeping just adds complexity.
    match damage_tracker.render_output(&mut renderer, &mut target, 0, &elements, [0.05, 0.05, 0.05, 1.0]) {
        Ok(result) => {
            // Flush pending GL commands so the compositor's write fence is submitted
            // promptly before the render target is unbound.  glFlush (non-blocking)
            // is sufficient — glFinish would stall the compositor thread unnecessarily.
            let _ = renderer.with_context(|gl| unsafe { gl.Flush() });
            let states = result.states;
            drop(target);
            state.gles_renderer = Some(renderer);
            let _ = state.frame_tx.send(CapturedFrame {
                rgba_buffer: None,
                dmabuf: Some(dmabuf_clone),
                drm_modifier,
                width,
                height,
                pts_ms: now_ms,
                captured_at,
            });
            state.encoded_frame_count += 1;
            Some(states)
        }
        Err(e) => {
            tracing::warn!("GlesRenderer render_output error: {:?}", e);
            drop(target);
            state.gles_renderer = Some(renderer);
            None
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
    captured_at: Instant,
    width: u32,
    height: u32,
) -> Option<RenderElementStates> {
    let mut renderer = match state.pixman_renderer.take() {
        Some(r) => r,
        None => return None,
    };

    let elements = match collect_elements_pixman(&mut renderer, &state.space, output) {
        Some(e) => e,
        None => { state.pixman_renderer = Some(renderer); return None; }
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
                return None;
            }
        }
    };

    let mut target = match renderer.bind(&mut pixman_img) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("PixmanRenderer bind error: {:?}", e);
            state.pixman_renderer = Some(renderer);
            return None;
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
        Ok(result) => {
            // Log element count and first pixel to diagnose black frames.
            if state.frame_counter % 150 == 0 {
                let first_pixel = if state.frame_buffer.len() >= 4 {
                    format!("BGRA({},{},{},{})",
                        state.frame_buffer[0], state.frame_buffer[1],
                        state.frame_buffer[2], state.frame_buffer[3])
                } else {
                    "?".into()
                };
                tracing::info!(
                    window_count = state.space.elements().count(),
                    first_pixel = %first_pixel,
                    "Pixman frame"
                );
            }
            let rgba = Bytes::copy_from_slice(&state.frame_buffer);
            let _ = state.frame_tx.send(CapturedFrame {
                rgba_buffer: Some(rgba),
                dmabuf: None,
                drm_modifier: 0,
                width,
                height,
                pts_ms: now_ms,
                captured_at,
            });
            state.encoded_frame_count += 1;
            Some(result.states)
        }
        Err(e) => {
            tracing::warn!("PixmanRenderer render_output error: {:?}", e);
            None
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
    match space.render_elements_for_output(renderer, output, 1.0) {
        Ok(elements) => {
            tracing::debug!(count = elements.len(), "Pixman render elements");
            Some(elements)
        }
        Err(e) => {
            tracing::warn!("render_elements_for_output (pixman) error: {:?}", e);
            None
        }
    }
}
