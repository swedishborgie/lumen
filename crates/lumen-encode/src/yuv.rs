//! RGBA → YUV420 color space conversion for the x264 software encoder.

/// Convert an RGBA (or BGRA/ARGB — any 4-byte-per-pixel) buffer to I420 (YUV 4:2:0 planar).
///
/// Input `rgba` must be exactly `width * height * 4` bytes.
/// Returns `(Y, U, V)` planes: Y is `width*height` bytes, U and V are each `(w/2)*(h/2)`.
// BT.601 math produces clamped i32 values that are safely cast to u8.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn rgba_to_i420(rgba: &[u8], width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let pixels = width * height;
    let mut y_plane = vec![0u8; pixels];
    let mut u_plane = vec![0u8; (width / 2) * (height / 2)];
    let mut v_plane = vec![0u8; (width / 2) * (height / 2)];

    for row in 0..height {
        for col in 0..width {
            let i = (row * width + col) * 4;
            let r = rgba[i] as i32;
            let g = rgba[i + 1] as i32;
            let b = rgba[i + 2] as i32;

            // BT.601 coefficients
            let yv = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * width + col] = yv.clamp(16, 235) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * (width / 2) + col / 2;
                let uv = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let vv = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                u_plane[uv_idx] = uv.clamp(16, 240) as u8;
                v_plane[uv_idx] = vv.clamp(16, 240) as u8;
            }
        }
    }

    (y_plane, u_plane, v_plane)
}
