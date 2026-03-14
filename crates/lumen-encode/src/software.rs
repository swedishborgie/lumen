//! x264 software H.264 encoder.
// FFI with x264-sys and ffmpeg-sys-next uses many intentional numeric casts.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::mem::MaybeUninit;

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use lumen_compositor::CapturedFrame;
use x264_sys::{
    x264_encoder_close, x264_encoder_encode, x264_encoder_intra_refresh, x264_encoder_open,
    x264_encoder_reconfig, x264_param_apply_profile, x264_param_default_preset, x264_picture_init,
    x264_picture_t, x264_t, X264_CSP_I420, X264_RC_ABR, X264_RC_CRF, X264_TYPE_IDR,
};

use crate::encoder::{EncodedFrame, VideoEncoder};
use crate::yuv::bgra_to_i420;

/// Software H.264 encoder backed by libx264.
pub struct SoftwareEncoder {
    handle: *mut x264_t,
    width: u32,
    height: u32,
    force_keyframe: bool,
    frame_index: i64,
    cbr: bool,
}

// x264_t is not Send by default (raw pointer), but we use it exclusively from
// a single thread, so this is safe.
unsafe impl Send for SoftwareEncoder {}

impl SoftwareEncoder {
    pub fn new(
        width: u32,
        height: u32,
        fps: f64,
        bitrate_kbps: u32,
        crf: i32,
        cbr: bool,
    ) -> Result<Self> {
        unsafe {
            let mut params = MaybeUninit::uninit();

            // "zerolatency" tuning minimises buffering — ideal for WebRTC.
            let preset = c"ultrafast".as_ptr();
            let tune = c"zerolatency".as_ptr();
            if x264_param_default_preset(params.as_mut_ptr(), preset, tune) != 0 {
                bail!("x264_param_default_preset failed");
            }
            let mut params = params.assume_init();

            params.i_width = width as i32;
            params.i_height = height as i32;
            params.i_fps_num = (fps * 1000.0) as u32;
            params.i_fps_den = 1000;
            params.i_threads = 1; // single-threaded for deterministic latency
            params.b_repeat_headers = 1; // SPS/PPS before every IDR
            params.b_annexb = 1; // Annex-B start codes

            if cbr {
                params.rc.i_rc_method = X264_RC_ABR as i32;
                params.rc.i_bitrate = bitrate_kbps as i32;
                params.rc.i_vbv_max_bitrate = bitrate_kbps as i32;
                params.rc.i_vbv_buffer_size = bitrate_kbps as i32; // 1-second buffer
            } else {
                params.rc.i_rc_method = X264_RC_CRF as i32;
                params.rc.f_rf_constant = crf as f32;
            }

            let profile = c"baseline".as_ptr();
            if x264_param_apply_profile(&mut params, profile) != 0 {
                bail!("x264_param_apply_profile failed");
            }

            let handle = x264_encoder_open(&mut params);
            if handle.is_null() {
                bail!("x264_encoder_open returned null");
            }

            Ok(Self { handle, width, height, force_keyframe: false, frame_index: 0, cbr })
        }
    }
}

impl Drop for SoftwareEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { x264_encoder_close(self.handle) };
        }
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>> {
        let rgba = match &frame.rgba_buffer {
            Some(r) => r.clone(),
            None => bail!("SoftwareEncoder requires an RGBA buffer (no DMA-BUF support)"),
        };

        if self.force_keyframe {
            unsafe { x264_encoder_intra_refresh(self.handle) };
            self.force_keyframe = false;
        }

        let (y, u, v) = bgra_to_i420(&rgba, self.width as usize, self.height as usize);

        let y_stride = self.width as i32;
        let uv_stride = (self.width / 2) as i32;

        let mut pic_in: x264_picture_t = unsafe {
            let mut p = MaybeUninit::uninit();
            x264_picture_init(p.as_mut_ptr());
            p.assume_init()
        };

        pic_in.img.i_csp = X264_CSP_I420 as i32;
        pic_in.img.i_plane = 3;
        pic_in.img.i_stride[0] = y_stride;
        pic_in.img.i_stride[1] = uv_stride;
        pic_in.img.i_stride[2] = uv_stride;
        pic_in.img.plane[0] = y.as_ptr() as *mut u8;
        pic_in.img.plane[1] = u.as_ptr() as *mut u8;
        pic_in.img.plane[2] = v.as_ptr() as *mut u8;
        pic_in.i_pts = self.frame_index;
        self.frame_index += 1;

        let mut pic_out: x264_picture_t = unsafe {
            let mut p = MaybeUninit::uninit();
            x264_picture_init(p.as_mut_ptr());
            p.assume_init()
        };

        let mut nal_ptr = std::ptr::null_mut();
        let mut nal_count: i32 = 0;

        let frame_size = unsafe {
            x264_encoder_encode(
                self.handle,
                &mut nal_ptr,
                &mut nal_count,
                &mut pic_in,
                &mut pic_out,
            )
        };

        if frame_size < 0 {
            return Err(anyhow!("x264_encoder_encode returned {}", frame_size));
        }
        if frame_size == 0 || nal_count == 0 {
            // Encoder is buffering; no output yet.
            return Ok(None);
        }

        // Collect all NAL units into a single Annex-B buffer.
        let is_keyframe = (pic_out.i_type as u32) == X264_TYPE_IDR;
        let mut data: Vec<u8> = Vec::with_capacity(frame_size as usize);
        for i in 0..nal_count as isize {
            let nal = unsafe { &*nal_ptr.offset(i) };
            let slice = unsafe {
                std::slice::from_raw_parts(nal.p_payload, nal.i_payload as usize)
            };
            data.extend_from_slice(slice);
        }

        Ok(Some(EncodedFrame {
            data: Bytes::from(data),
            pts_ms: frame.pts_ms,
            is_keyframe,
            captured_at: frame.captured_at,
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn update_bitrate(&mut self, kbps: u32) {
        if !self.cbr {
            return;
        }
        unsafe {
            let mut params = MaybeUninit::uninit();
            // x264_encoder_parameters fills our local copy of the current params.
            x264_sys::x264_encoder_parameters(self.handle, params.as_mut_ptr());
            let mut params = params.assume_init();
            params.rc.i_bitrate = kbps as i32;
            params.rc.i_vbv_max_bitrate = kbps as i32;
            params.rc.i_vbv_buffer_size = kbps as i32;
            x264_encoder_reconfig(self.handle, &mut params);
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        // Close the current encoder and reinitialize with new dimensions.
        // Bitrate and fps are preserved; we need them for the new encoder params.
        unsafe {
            // Read current params to preserve fps and rc settings.
            let mut params = MaybeUninit::uninit();
            x264_sys::x264_encoder_parameters(self.handle, params.as_mut_ptr());
            let mut params = params.assume_init();

            // Close old encoder.
            x264_encoder_close(self.handle);
            self.handle = std::ptr::null_mut();

            // Apply new dimensions.
            params.i_width = width as i32;
            params.i_height = height as i32;

            let profile = c"baseline".as_ptr();
            if x264_param_apply_profile(&mut params, profile) != 0 {
                anyhow::bail!("x264_param_apply_profile failed during resize");
            }

            let handle = x264_encoder_open(&mut params);
            if handle.is_null() {
                anyhow::bail!("x264_encoder_open returned null during resize");
            }

            self.handle = handle;
            self.width = width;
            self.height = height;
            self.frame_index = 0;
            self.force_keyframe = true;
        }
        Ok(())
    }
}
