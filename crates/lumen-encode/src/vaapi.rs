//! VA-API hardware H.264 encoder using ffmpeg-sys-next.
//!
//! Pipeline: RGBA CPU → NV12 (sws_scale) → VA-API HW frame (av_hwframe_transfer_data) → h264_vaapi
// FFI with ffmpeg-sys-next uses many intentional numeric casts.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use ffmpeg_sys_next::*;
use lumen_compositor::types::CapturedFrame;

use crate::encoder::{EncodedFrame, VideoEncoder};

/// Safety wrapper that calls av_frame_free on drop.
struct AvFramePtr(*mut AVFrame);
impl AvFramePtr {
    unsafe fn alloc() -> Result<Self> {
        let p = av_frame_alloc();
        if p.is_null() { bail!("av_frame_alloc failed"); }
        Ok(Self(p))
    }
    fn as_mut(&mut self) -> *mut AVFrame { self.0 }
    fn as_ptr(&self) -> *const AVFrame { self.0 }
}
impl Drop for AvFramePtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { av_frame_free(&mut self.0); }
        }
    }
}

/// Safety wrapper that calls av_packet_free on drop.
struct AvPacketPtr(*mut AVPacket);
impl AvPacketPtr {
    unsafe fn alloc() -> Result<Self> {
        let p = av_packet_alloc();
        if p.is_null() { bail!("av_packet_alloc failed"); }
        Ok(Self(p))
    }
    fn as_mut(&mut self) -> *mut AVPacket { self.0 }
}
impl Drop for AvPacketPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { av_packet_free(&mut self.0); }
        }
    }
}

pub struct VaapiEncoder {
    codec_ctx: *mut AVCodecContext,
    hw_device_ctx: *mut AVBufferRef,
    hw_frames_ctx: *mut AVBufferRef,
    sws_ctx: *mut SwsContext,
    keyframe_requested: Arc<AtomicBool>,
    width: i32,
    height: i32,
    frame_index: i64,
}

// SAFETY: The raw pointers are only accessed from the encoder task thread.
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    pub fn new(config: &crate::encoder::EncoderConfig) -> Result<Self> {
        let render_node = config.render_node.as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/dev/dri/renderD128".to_owned());

        unsafe {
            // ----------------------------------------------------------------
            // 1. Create VA-API hardware device context
            // ----------------------------------------------------------------
            let device_path = std::ffi::CString::new(render_node.clone())
                .context("Invalid render node path")?;
            let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
            let ret = av_hwdevice_ctx_create(
                &mut hw_device_ctx,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                device_path.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if ret < 0 {
                bail!("av_hwdevice_ctx_create failed for {}: error {}", render_node, ret);
            }

            // ----------------------------------------------------------------
            // 2. Find h264_vaapi encoder
            // ----------------------------------------------------------------
            let codec_name = c"h264_vaapi".as_ptr();
            let codec = avcodec_find_encoder_by_name(codec_name);
            if codec.is_null() {
                av_buffer_unref(&mut hw_device_ctx);
                bail!("h264_vaapi codec not found — is ffmpeg built with VA-API support?");
            }

            // ----------------------------------------------------------------
            // 3. Allocate codec context and set parameters
            // ----------------------------------------------------------------
            let codec_ctx = avcodec_alloc_context3(codec);
            if codec_ctx.is_null() {
                av_buffer_unref(&mut hw_device_ctx);
                bail!("avcodec_alloc_context3 failed");
            }

            let w = config.width as i32;
            let h = config.height as i32;
            (*codec_ctx).width = w;
            (*codec_ctx).height = h;
            (*codec_ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*codec_ctx).time_base = AVRational { num: 1, den: 1000 }; // ms timebase
            (*codec_ctx).framerate = AVRational { num: config.fps as i32, den: 1 };
            (*codec_ctx).gop_size = (config.fps * 2.0) as i32; // keyframe every 2s
            (*codec_ctx).max_b_frames = 0; // no B-frames for low latency
            (*codec_ctx).bit_rate = (config.bitrate_kbps as i64) * 1000;
            // Attach the hardware device context
            (*codec_ctx).hw_device_ctx = av_buffer_ref(hw_device_ctx);

            // ----------------------------------------------------------------
            // 4. Create hardware frames context (NV12 SW, VAAPI HW)
            // ----------------------------------------------------------------
            let hw_frames_ref = av_hwframe_ctx_alloc(hw_device_ctx);
            if hw_frames_ref.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_device_ctx);
                bail!("av_hwframe_ctx_alloc failed");
            }

            {
                let frames_ctx = (*hw_frames_ref).data as *mut AVHWFramesContext;
                (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
                (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*frames_ctx).width = w;
                (*frames_ctx).height = h;
                (*frames_ctx).initial_pool_size = 8;
            }

            let ret = av_hwframe_ctx_init(hw_frames_ref);
            if ret < 0 {
                av_buffer_unref(&mut hw_device_ctx);
                bail!("av_hwframe_ctx_init failed: {}", ret);
            }

            (*codec_ctx).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
            let hw_frames_ctx = hw_frames_ref; // transfer ownership to struct

            // ----------------------------------------------------------------
            // 5. Open the encoder
            // ----------------------------------------------------------------
            // local_header: embed SPS/PPS in each IDR, needed for WebRTC.
            (*codec_ctx).flags |= AV_CODEC_FLAG2_LOCAL_HEADER;
            let mut opts: *mut AVDictionary = std::ptr::null_mut();
            av_dict_set(&mut opts, c"profile".as_ptr(), c"high".as_ptr(), 0);
            av_dict_set(&mut opts, c"level".as_ptr(), c"4.1".as_ptr(), 0);
            // rc_mode 2 = CBR
            av_dict_set(&mut opts, c"rc_mode".as_ptr(), c"CBR".as_ptr(), 0);

            let ret = avcodec_open2(codec_ctx, codec, &mut opts);
            if !opts.is_null() {
                av_dict_free(&mut opts);
            }
            if ret < 0 {
                av_buffer_unref(&mut hw_device_ctx);
                bail!("avcodec_open2 failed: {}", ret);
            }

            // ----------------------------------------------------------------
            // 6. Set up software → hardware pixel format conversion (RGBA → NV12)
            // ----------------------------------------------------------------
            let sws_ctx = sws_getContext(
                w, h, AVPixelFormat::AV_PIX_FMT_RGBA,
                w, h, AVPixelFormat::AV_PIX_FMT_NV12,
                SWS_BILINEAR,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if sws_ctx.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_device_ctx);
                bail!("sws_getContext failed");
            }

            tracing::info!("VA-API encoder initialised on {}", render_node);

            Ok(Self {
                codec_ctx,
                hw_device_ctx,
                hw_frames_ctx,
                sws_ctx,
                keyframe_requested: Arc::new(AtomicBool::new(false)),
                width: w,
                height: h,
                frame_index: 0,
            })
        }
    }

    /// Convert RGBA bytes to a software NV12 AVFrame.
    unsafe fn rgba_to_nv12_frame(&mut self, rgba: &[u8], pts_ms: u64) -> Result<AvFramePtr> {
        let mut sw_frame = AvFramePtr::alloc()?;
        let f = sw_frame.as_mut();
        (*f).width = self.width;
        (*f).height = self.height;
        (*f).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
        let ret = av_frame_get_buffer(f, 0);
        if ret < 0 { bail!("av_frame_get_buffer (sw) failed: {}", ret); }

        // sws_scale expects src as array of plane pointers
        let src_data = [rgba.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null()];
        let src_linesize = [self.width * 4, 0, 0, 0];

        sws_scale(
            self.sws_ctx,
            src_data.as_ptr(),
            src_linesize.as_ptr(),
            0,
            self.height,
            (*f).data.as_ptr() as *mut *mut u8,
            (*f).linesize.as_ptr() as *mut i32,
        );

        (*f).pts = pts_ms as i64;
        Ok(sw_frame)
    }

    /// Upload a SW NV12 frame to a VA-API HW frame.
    unsafe fn upload_to_hw(&mut self, sw_frame: &AvFramePtr) -> Result<AvFramePtr> {
        let mut hw_frame = AvFramePtr::alloc()?;
        let ret = av_hwframe_get_buffer(self.hw_frames_ctx, hw_frame.as_mut(), 0);
        if ret < 0 { bail!("av_hwframe_get_buffer failed: {}", ret); }

        let ret = av_hwframe_transfer_data(hw_frame.as_mut(), sw_frame.as_ptr(), 0);
        if ret < 0 { bail!("av_hwframe_transfer_data failed: {}", ret); }

        (*hw_frame.as_mut()).pts = (*sw_frame.as_ptr()).pts;
        Ok(hw_frame)
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.sws_ctx.is_null() { sws_freeContext(self.sws_ctx); }
            if !self.codec_ctx.is_null() { avcodec_free_context(&mut (self.codec_ctx as *mut _)); }
            if !self.hw_frames_ctx.is_null() { av_buffer_unref(&mut self.hw_frames_ctx); }
            if !self.hw_device_ctx.is_null() { av_buffer_unref(&mut self.hw_device_ctx); }
        }
    }
}

impl VideoEncoder for VaapiEncoder {
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>> {
        let rgba = match frame.rgba_buffer {
            Some(buf) => buf,
            None => bail!("VA-API encoder requires RGBA CPU buffer"),
        };

        if self.keyframe_requested.swap(false, Ordering::Relaxed) {
            // There is no direct IDR-force API in h264_vaapi; rely on periodic
            // IDR from gop_size and inband SPS/PPS for WebRTC recovery.
            tracing::debug!("Keyframe requested (VA-API: will emit on next GOP boundary)");
        }

        unsafe {
            let sw_frame = self.rgba_to_nv12_frame(&rgba, frame.pts_ms)?;
            let mut hw_frame = self.upload_to_hw(&sw_frame)?;

            let ret = avcodec_send_frame(self.codec_ctx, hw_frame.as_mut());
            if ret < 0 && ret != AVERROR(EAGAIN) {
                bail!("avcodec_send_frame failed: {}", ret);
            }

            let mut pkt = AvPacketPtr::alloc()?;
            let ret = avcodec_receive_packet(self.codec_ctx, pkt.as_mut());
            if ret == AVERROR(EAGAIN) || ret == AVERROR_EOF {
                return Ok(None);
            }
            if ret < 0 {
                bail!("avcodec_receive_packet failed: {}", ret);
            }

            let p = &*pkt.as_mut();
            let data = std::slice::from_raw_parts(p.data, p.size as usize);
            let is_keyframe = (p.flags & AV_PKT_FLAG_KEY) != 0;
            let pts_ms = if p.pts != AV_NOPTS_VALUE { p.pts as u64 } else { frame.pts_ms };
            self.frame_index += 1;

            Ok(Some(EncodedFrame {
                data: Bytes::copy_from_slice(data),
                pts_ms,
                is_keyframe,
            }))
        }
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    fn update_bitrate(&mut self, kbps: u32) {
        unsafe {
            (*self.codec_ctx).bit_rate = (kbps as i64) * 1000;
        }
    }
}
