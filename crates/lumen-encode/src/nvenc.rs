//! NVIDIA NVENC hardware H.264 encoder using ffmpeg-sys-next.
//!
//! Pipeline (zero-copy): DMA-BUF (ARGB8888) → DRM_PRIME frame → hwmap → scale_cuda (NV12) → h264_nvenc
//!
//! Always paired with the GPU compositor path (`--dri-node`).  The compositor
//! produces DMA-BUF frames via GBM/GlesRenderer; these are imported directly
//! into CUDA via FFmpeg's `hwmap` filter using CUDA external memory (no
//! GPU→CPU→GPU round-trip).  Requires NVIDIA driver 470+ for DMA-BUF import
//! and FFmpeg built with `--enable-nvenc --enable-cuda`.
// FFI with ffmpeg-sys-next uses many intentional numeric casts.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::collections::VecDeque;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use ffmpeg_sys_next::*;
use libc::c_void;
use lumen_compositor::types::CapturedFrame;

use crate::encoder::{EncodedFrame, VideoEncoder};

// DRM fourcc for ARGB8888 ('A','R','2','4' in little-endian = [B,G,R,A] bytes)
const DRM_FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");

/// Safety wrapper that calls av_frame_free on drop.
struct AvFramePtr(*mut AVFrame);
impl AvFramePtr {
    unsafe fn alloc() -> Result<Self> {
        let p = av_frame_alloc();
        if p.is_null() { bail!("av_frame_alloc failed"); }
        Ok(Self(p))
    }
    fn as_mut(&mut self) -> *mut AVFrame { self.0 }
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

pub struct NvencEncoder {
    codec_ctx: *mut AVCodecContext,
    cuda_device_ctx: *mut AVBufferRef,    // CUDA device for NVENC encoding
    hw_frames_ctx: *mut AVBufferRef,      // CUDA NV12 frames pool for the encoder

    // Zero-copy DMA-BUF path
    drm_device_ctx: *mut AVBufferRef,     // DRM device context (DRM_PRIME frame import)
    drm_frames_ctx: *mut AVBufferRef,     // DRM hw_frames_ctx (ARGB8888/BGRA, no pool)
    filter_graph: *mut AVFilterGraph,
    filter_buffersrc: *mut AVFilterContext,
    filter_buffersink: *mut AVFilterContext,

    keyframe_requested: Arc<AtomicBool>,
    width: i32,
    height: i32,
    frame_index: i64,
    /// FIFO of capture instants for frames submitted to the encoder.
    ///
    /// Pushed when `avcodec_send_frame` succeeds, popped when
    /// `avcodec_receive_packet` returns a packet.  Because `max_b_frames = 0`
    /// and no reordering occurs, the queue is strictly FIFO.  This lets
    /// `push_video` pass the original capture `Instant` — not `Instant::now()`
    /// — to `writer.write()`, which is critical for correct RTCP SR timestamps.
    pending_captured_at: VecDeque<Instant>,

    // Stored for use during resize reinitialization.
    fps: f64,
    bitrate_kbps: u32,
    max_bitrate_kbps: u32,
}

// SAFETY: raw pointers are only accessed from the single encoder task thread.
unsafe impl Send for NvencEncoder {}

impl NvencEncoder {
    pub fn new(config: &crate::encoder::EncoderConfig) -> Result<Self> {
        let cuda_device = config
            .cuda_device
            .as_deref()
            .filter(|d| !d.is_empty())
            .unwrap_or("0");

        let render_node = config
            .render_node
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/dev/dri/renderD128".to_owned());

        let w = config.width as i32;
        let h = config.height as i32;

        unsafe {
            let cuda_device_cstr = CString::new(cuda_device)
                .context("Invalid CUDA device string")?;
            let drm_path_cstr = CString::new(render_node.as_str())
                .context("Invalid render node path")?;

            // ----------------------------------------------------------------
            // 1. CUDA device context (for NVENC encoding)
            // ----------------------------------------------------------------
            let mut cuda_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
            let ret = av_hwdevice_ctx_create(
                &mut cuda_device_ctx,
                AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                cuda_device_cstr.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if ret < 0 {
                bail!(
                    "av_hwdevice_ctx_create (CUDA) failed for device {}: error {}; \
                     ensure NVIDIA drivers are installed and FFmpeg has CUDA support",
                    cuda_device, ret
                );
            }

            // ----------------------------------------------------------------
            // 2. DRM device context (needed to wrap DMA-BUF frames as DRM_PRIME)
            // ----------------------------------------------------------------
            let mut drm_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
            let ret = av_hwdevice_ctx_create(
                &mut drm_device_ctx,
                AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
                drm_path_cstr.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if ret < 0 {
                av_buffer_unref(&mut cuda_device_ctx);
                bail!(
                    "av_hwdevice_ctx_create (DRM) failed for {}: error {}",
                    render_node, ret
                );
            }

            // ----------------------------------------------------------------
            // 3. Find h264_nvenc encoder and allocate codec context
            // ----------------------------------------------------------------
            let codec = avcodec_find_encoder_by_name(c"h264_nvenc".as_ptr());
            if codec.is_null() {
                av_buffer_unref(&mut cuda_device_ctx);
                av_buffer_unref(&mut drm_device_ctx);
                bail!(
                    "h264_nvenc codec not found — is FFmpeg built with NVENC support \
                     and are NVIDIA drivers installed?"
                );
            }

            let codec_ctx = avcodec_alloc_context3(codec);
            if codec_ctx.is_null() {
                av_buffer_unref(&mut cuda_device_ctx);
                av_buffer_unref(&mut drm_device_ctx);
                bail!("avcodec_alloc_context3 failed");
            }

            (*codec_ctx).width = w;
            (*codec_ctx).height = h;
            (*codec_ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_CUDA;
            (*codec_ctx).time_base = AVRational { num: 1, den: 1000 };
            (*codec_ctx).framerate = AVRational { num: config.fps as i32, den: 1 };
            (*codec_ctx).gop_size = (config.fps * 2.0) as i32;
            (*codec_ctx).max_b_frames = 0;
            (*codec_ctx).bit_rate = (config.bitrate_kbps as i64) * 1000;
            (*codec_ctx).rc_max_rate = (config.max_bitrate_kbps as i64) * 1000;
            (*codec_ctx).hw_device_ctx = av_buffer_ref(cuda_device_ctx);

            // ----------------------------------------------------------------
            // 4. Encoder hw_frames_ctx — NV12 CUDA pool
            // ----------------------------------------------------------------
            let mut hw_frames_ref = av_hwframe_ctx_alloc(cuda_device_ctx);
            if hw_frames_ref.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut cuda_device_ctx);
                av_buffer_unref(&mut drm_device_ctx);
                bail!("av_hwframe_ctx_alloc (CUDA) failed");
            }
            {
                let fc = (*hw_frames_ref).data as *mut AVHWFramesContext;
                (*fc).format = AVPixelFormat::AV_PIX_FMT_CUDA;
                (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*fc).width = w;
                (*fc).height = h;
                (*fc).initial_pool_size = 8;
            }
            let ret = av_hwframe_ctx_init(hw_frames_ref);
            if ret < 0 {
                av_buffer_unref(&mut hw_frames_ref);
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut cuda_device_ctx);
                av_buffer_unref(&mut drm_device_ctx);
                bail!("av_hwframe_ctx_init (CUDA encoder) failed: {}", ret);
            }
            (*codec_ctx).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
            let hw_frames_ctx = hw_frames_ref;

            // ----------------------------------------------------------------
            // 5. Open the encoder
            //
            // preset=p1:  fastest NVENC preset — minimises encode latency.
            // tune=ll:    low-latency tuning — sets delay=0 internally, which
            //             matches the VAAPI async_depth=1 goal: no extra frames
            //             buffered inside the encoder, so the pending_captured_at
            //             FIFO stays accurate and RTCP SR timestamps are correct.
            // rc=vbr:     variable bitrate, matching the VAAPI VBR mode.
            // ----------------------------------------------------------------
            (*codec_ctx).flags |= AV_CODEC_FLAG2_LOCAL_HEADER;
            let mut opts: *mut AVDictionary = std::ptr::null_mut();
            av_dict_set(&mut opts, c"profile".as_ptr(), c"high".as_ptr(), 0);
            av_dict_set(&mut opts, c"preset".as_ptr(), c"p1".as_ptr(), 0);
            av_dict_set(&mut opts, c"tune".as_ptr(), c"ll".as_ptr(), 0);
            av_dict_set(&mut opts, c"rc".as_ptr(), c"vbr".as_ptr(), 0);
            let ret = avcodec_open2(codec_ctx, codec, &mut opts);
            if !opts.is_null() { av_dict_free(&mut opts); }
            if ret < 0 {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_frames_ref);
                av_buffer_unref(&mut cuda_device_ctx);
                av_buffer_unref(&mut drm_device_ctx);
                bail!("avcodec_open2 (h264_nvenc) failed: {}", ret);
            }

            // ----------------------------------------------------------------
            // 6. DRM hw_frames_ctx and filter graph for the DMA-BUF zero-copy path
            // ----------------------------------------------------------------
            let (drm_frames_ctx, filter_graph, filter_buffersrc, filter_buffersink) =
                match init_dmabuf_pipeline(drm_device_ctx, cuda_device_ctx, w, h, config.fps) {
                    Ok((dfc, fg, src, sink)) => {
                        tracing::info!("NVENC zero-copy DMA-BUF pipeline ready");
                        (dfc, fg, src, sink)
                    }
                    Err(e) => {
                        avcodec_free_context(&mut (codec_ctx as *mut _));
                        av_buffer_unref(&mut hw_frames_ref);
                        av_buffer_unref(&mut cuda_device_ctx);
                        av_buffer_unref(&mut drm_device_ctx);
                        return Err(e.context("NVENC DMA-BUF pipeline setup failed"));
                    }
                };

            tracing::info!("NVENC encoder initialised (CUDA device {}, DRI {})", cuda_device, render_node);

            Ok(Self {
                codec_ctx,
                cuda_device_ctx,
                hw_frames_ctx,
                drm_device_ctx,
                drm_frames_ctx,
                filter_graph,
                filter_buffersrc,
                filter_buffersink,
                keyframe_requested: Arc::new(AtomicBool::new(false)),
                width: w,
                height: h,
                frame_index: 0,
                pending_captured_at: VecDeque::new(),
                fps: config.fps,
                bitrate_kbps: config.bitrate_kbps,
                max_bitrate_kbps: config.max_bitrate_kbps,
            })
        }
    }

    /// Push a DMA-BUF through the filter graph (hwmap → scale_cuda) to obtain
    /// a CUDA NV12 frame, then encode it with h264_nvenc.  Zero GPU→CPU copies.
    unsafe fn encode_from_dmabuf(
        &mut self,
        frame: &CapturedFrame,
    ) -> Result<Option<EncodedFrame>> {
        use std::os::unix::io::AsRawFd;

        let dmabuf = frame.dmabuf.as_ref().expect("called encode_from_dmabuf without dmabuf");
        let fd = dmabuf.handles().next()
            .context("DMA-BUF has no handles")?
            .as_raw_fd();
        let stride = dmabuf.strides().next().context("DMA-BUF has no strides")? as isize;
        let offset = dmabuf.offsets().next().context("DMA-BUF has no offsets")? as isize;
        let size = stride as usize * self.height as usize;

        // Build AVDRMFrameDescriptor on the heap; owned by the AVBufferRef below.
        let desc = av_mallocz(std::mem::size_of::<AVDRMFrameDescriptor>())
            as *mut AVDRMFrameDescriptor;
        if desc.is_null() { bail!("av_mallocz for AVDRMFrameDescriptor failed"); }

        (*desc).nb_objects = 1;
        (*desc).objects[0].fd = fd;
        (*desc).objects[0].size = size;
        (*desc).objects[0].format_modifier = frame.drm_modifier;

        (*desc).nb_layers = 1;
        (*desc).layers[0].format = DRM_FORMAT_ARGB8888;
        (*desc).layers[0].nb_planes = 1;
        (*desc).layers[0].planes[0].object_index = 0;
        (*desc).layers[0].planes[0].offset = offset;
        (*desc).layers[0].planes[0].pitch = stride;

        let mut drm_frame = AvFramePtr::alloc()?;
        let f = drm_frame.as_mut();
        (*f).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        (*f).width = self.width;
        (*f).height = self.height;
        (*f).pts = frame.pts_ms as i64;
        (*f).hw_frames_ctx = av_buffer_ref(self.drm_frames_ctx);
        (*f).data[0] = desc as *mut u8;
        (*f).buf[0] = av_buffer_create(
            desc as *mut u8,
            std::mem::size_of::<AVDRMFrameDescriptor>(),
            Some(free_drm_desc),
            std::ptr::null_mut(),
            AV_BUFFER_FLAG_READONLY,
        );

        // Hand the DRM_PRIME frame to the filter graph (hwmap + scale_cuda).
        let ret = av_buffersrc_add_frame_flags(
            self.filter_buffersrc, f, AV_BUFFERSRC_FLAG_KEEP_REF as i32,
        );
        if ret < 0 { bail!("av_buffersrc_add_frame_flags failed: {}", ret); }

        // Receive the resulting CUDA NV12 frame.
        let mut nv12_frame = AvFramePtr::alloc()?;
        let ret = av_buffersink_get_frame(self.filter_buffersink, nv12_frame.as_mut());
        if ret == AVERROR(EAGAIN) { return Ok(None); }
        if ret < 0 { bail!("av_buffersink_get_frame failed: {}", ret); }

        self.send_and_receive(&mut nv12_frame, frame.pts_ms, frame.captured_at)
    }

    /// Submit a hardware frame to the encoder and drain one packet.
    unsafe fn send_and_receive(
        &mut self,
        hw_frame: &mut AvFramePtr,
        fallback_pts_ms: u64,
        captured_at: Instant,
    ) -> Result<Option<EncodedFrame>> {
        let ret = avcodec_send_frame(self.codec_ctx, hw_frame.as_mut());
        if ret < 0 && ret != AVERROR(EAGAIN) {
            bail!("avcodec_send_frame failed: {}", ret);
        }
        // Record capture instant so we can attach it to the output packet,
        // regardless of how many frames the encoder holds internally.
        self.pending_captured_at.push_back(captured_at);

        let mut pkt = AvPacketPtr::alloc()?;
        let ret = avcodec_receive_packet(self.codec_ctx, pkt.as_mut());
        if ret == AVERROR(EAGAIN) || ret == AVERROR_EOF { return Ok(None); }
        if ret < 0 { bail!("avcodec_receive_packet failed: {}", ret); }

        let p = &*pkt.as_mut();
        let data = std::slice::from_raw_parts(p.data, p.size as usize);
        let is_keyframe = (p.flags & AV_PKT_FLAG_KEY) != 0;
        let pts_ms = if p.pts != AV_NOPTS_VALUE { p.pts as u64 } else { fallback_pts_ms };
        // Pop the oldest pending capture instant (FIFO; no B-frames so order is preserved).
        let frame_captured_at = self.pending_captured_at.pop_front().unwrap_or(captured_at);
        self.frame_index += 1;

        Ok(Some(EncodedFrame {
            data: Bytes::copy_from_slice(data),
            pts_ms,
            is_keyframe,
            captured_at: frame_captured_at,
        }))
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.filter_graph.is_null() { avfilter_graph_free(&mut self.filter_graph); }
            if !self.codec_ctx.is_null() { avcodec_free_context(&mut (self.codec_ctx as *mut _)); }
            if !self.hw_frames_ctx.is_null() { av_buffer_unref(&mut self.hw_frames_ctx); }
            if !self.drm_frames_ctx.is_null() { av_buffer_unref(&mut self.drm_frames_ctx); }
            if !self.cuda_device_ctx.is_null() { av_buffer_unref(&mut self.cuda_device_ctx); }
            if !self.drm_device_ctx.is_null() { av_buffer_unref(&mut self.drm_device_ctx); }
        }
    }
}

impl VideoEncoder for NvencEncoder {
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>> {
        if self.keyframe_requested.swap(false, Ordering::Relaxed) {
            tracing::debug!("Keyframe requested (NVENC: will emit on next GOP boundary)");
        }

        unsafe {
            if frame.dmabuf.is_some() {
                self.encode_from_dmabuf(&frame)
            } else {
                // NvencEncoder is only selected when a DRI node is available, which
                // enables GPU rendering.  Receiving an RGBA frame here indicates an
                // unexpected compositor state.  The software x264 encoder handles RGBA.
                bail!("NvencEncoder received an RGBA frame; use the software encoder for CPU rendering");
            }
        }
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    fn update_bitrate(&mut self, kbps: u32) {
        let ratio = self.max_bitrate_kbps as f64 / self.bitrate_kbps.max(1) as f64;
        self.bitrate_kbps = kbps;
        self.max_bitrate_kbps = ((kbps as f64 * ratio).round() as u32).max(kbps);
        unsafe {
            (*self.codec_ctx).bit_rate = (kbps as i64) * 1000;
            (*self.codec_ctx).rc_max_rate = (self.max_bitrate_kbps as i64) * 1000;
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        let w = width as i32;
        let h = height as i32;

        unsafe {
            // --- Free resolution-dependent resources ---
            if !self.filter_graph.is_null() {
                avfilter_graph_free(&mut self.filter_graph);
                self.filter_graph = std::ptr::null_mut();
                self.filter_buffersrc = std::ptr::null_mut();
                self.filter_buffersink = std::ptr::null_mut();
            }
            if !self.drm_frames_ctx.is_null() {
                av_buffer_unref(&mut self.drm_frames_ctx);
            }
            if !self.codec_ctx.is_null() {
                avcodec_free_context(&mut (self.codec_ctx as *mut _));
                self.codec_ctx = std::ptr::null_mut();
            }
            if !self.hw_frames_ctx.is_null() {
                av_buffer_unref(&mut self.hw_frames_ctx);
                self.hw_frames_ctx = std::ptr::null_mut();
            }

            // --- Rebuild codec context ---
            let codec = avcodec_find_encoder_by_name(c"h264_nvenc".as_ptr());
            if codec.is_null() {
                anyhow::bail!("h264_nvenc codec not found during resize");
            }
            let codec_ctx = avcodec_alloc_context3(codec);
            if codec_ctx.is_null() {
                anyhow::bail!("avcodec_alloc_context3 failed during resize");
            }

            (*codec_ctx).width = w;
            (*codec_ctx).height = h;
            (*codec_ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_CUDA;
            (*codec_ctx).time_base = AVRational { num: 1, den: 1000 };
            (*codec_ctx).framerate = AVRational { num: self.fps as i32, den: 1 };
            (*codec_ctx).gop_size = (self.fps * 2.0) as i32;
            (*codec_ctx).max_b_frames = 0;
            (*codec_ctx).bit_rate = (self.bitrate_kbps as i64) * 1000;
            (*codec_ctx).rc_max_rate = (self.max_bitrate_kbps as i64) * 1000;
            (*codec_ctx).hw_device_ctx = av_buffer_ref(self.cuda_device_ctx);

            // --- Rebuild hw_frames_ctx (NV12 CUDA pool) ---
            let mut hw_frames_ref = av_hwframe_ctx_alloc(self.cuda_device_ctx);
            if hw_frames_ref.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                anyhow::bail!("av_hwframe_ctx_alloc (CUDA) failed during resize");
            }
            {
                let fc = (*hw_frames_ref).data as *mut AVHWFramesContext;
                (*fc).format = AVPixelFormat::AV_PIX_FMT_CUDA;
                (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*fc).width = w;
                (*fc).height = h;
                (*fc).initial_pool_size = 8;
            }
            let ret = av_hwframe_ctx_init(hw_frames_ref);
            if ret < 0 {
                av_buffer_unref(&mut hw_frames_ref);
                avcodec_free_context(&mut (codec_ctx as *mut _));
                anyhow::bail!("av_hwframe_ctx_init (CUDA) failed during resize: {}", ret);
            }
            (*codec_ctx).hw_frames_ctx = av_buffer_ref(hw_frames_ref);

            (*codec_ctx).flags |= AV_CODEC_FLAG2_LOCAL_HEADER;
            let mut opts: *mut AVDictionary = std::ptr::null_mut();
            av_dict_set(&mut opts, c"profile".as_ptr(), c"high".as_ptr(), 0);
            av_dict_set(&mut opts, c"preset".as_ptr(), c"p1".as_ptr(), 0);
            av_dict_set(&mut opts, c"tune".as_ptr(), c"ll".as_ptr(), 0);
            av_dict_set(&mut opts, c"rc".as_ptr(), c"vbr".as_ptr(), 0);
            let ret = avcodec_open2(codec_ctx, codec, &mut opts);
            if !opts.is_null() { av_dict_free(&mut opts); }
            if ret < 0 {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_frames_ref);
                anyhow::bail!("avcodec_open2 (h264_nvenc) failed during resize: {}", ret);
            }

            self.codec_ctx = codec_ctx;
            self.hw_frames_ctx = hw_frames_ref;

            // --- Rebuild DMA-BUF filter pipeline ---
            match init_dmabuf_pipeline(self.drm_device_ctx, self.cuda_device_ctx, w, h, self.fps) {
                Ok((dfc, fg, src, sink)) => {
                    self.drm_frames_ctx = dfc;
                    self.filter_graph = fg;
                    self.filter_buffersrc = src;
                    self.filter_buffersink = sink;
                }
                Err(e) => {
                    anyhow::bail!("NVENC DMA-BUF pipeline rebuild failed after resize: {}", e);
                }
            }

            self.width = w;
            self.height = h;
            self.frame_index = 0;
            self.pending_captured_at.clear();
        }

        tracing::info!("NVENC encoder resized to {}x{}", width, height);
        Ok(())
    }
}

/// Free callback for the AVBufferRef wrapping an AVDRMFrameDescriptor.
unsafe extern "C" fn free_drm_desc(_opaque: *mut c_void, data: *mut u8) {
    av_free(data as *mut c_void);
}

/// Build the DRM hw_frames_ctx and the filter graph:
///   buffer (DRM_PRIME / BGRA)  →  hwmap (→ CUDA)  →  scale_cuda (NV12)  →  buffersink
///
/// The `hwmap` filter maps the DMA-BUF into a CUDA surface using CUDA external
/// memory import (NVIDIA driver 470+), achieving true zero-copy: pixel data
/// never passes through the CPU.
///
/// Returns `(drm_frames_ctx, filter_graph, buffersrc_ctx, buffersink_ctx)`.
unsafe fn init_dmabuf_pipeline(
    drm_device_ctx: *mut AVBufferRef,
    cuda_device_ctx: *mut AVBufferRef,
    w: i32,
    h: i32,
    fps: f64,
) -> Result<(*mut AVBufferRef, *mut AVFilterGraph, *mut AVFilterContext, *mut AVFilterContext)> {
    // DRM hw_frames_ctx: no pool (pool_size=0), format metadata only.
    // Incoming frames supply their own AVDRMFrameDescriptor via data[0]/buf[0].
    let drm_frames_ref = av_hwframe_ctx_alloc(drm_device_ctx);
    if drm_frames_ref.is_null() { bail!("av_hwframe_ctx_alloc (DRM) failed"); }
    {
        let fc = (*drm_frames_ref).data as *mut AVHWFramesContext;
        (*fc).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME;
        (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_BGRA; // ARGB8888 = [B,G,R,A] in memory
        (*fc).width = w;
        (*fc).height = h;
        (*fc).initial_pool_size = 0;
    }
    let ret = av_hwframe_ctx_init(drm_frames_ref);
    if ret < 0 {
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("av_hwframe_ctx_init (DRM) failed: {}", ret);
    }

    // Allocate filter graph.
    let graph = avfilter_graph_alloc();
    if graph.is_null() {
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_alloc failed");
    }

    // buffer source: pix_fmt=DRM_PRIME, carries hw_frames_ctx set via parameters.
    let buffersrc_args = CString::new(format!(
        "video_size={}x{}:pix_fmt={}:time_base=1/1000:frame_rate={}/1",
        w, h, AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32, fps as i32,
    )).expect("CString");
    let mut src_ctx: *mut AVFilterContext = std::ptr::null_mut();
    let ret = avfilter_graph_create_filter(
        &mut src_ctx,
        avfilter_get_by_name(c"buffer".as_ptr()),
        c"in".as_ptr(),
        buffersrc_args.as_ptr(),
        std::ptr::null_mut(),
        graph,
    );
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_create_filter (buffer) failed: {}", ret);
    }

    // Tell buffersrc about the DRM hw_frames_ctx so downstream filters see it.
    let params = av_buffersrc_parameters_alloc();
    if !params.is_null() {
        (*params).hw_frames_ctx = av_buffer_ref(drm_frames_ref);
        av_buffersrc_parameters_set(src_ctx, params);
        av_free(params as *mut c_void);
    }

    // hwmap: DRM_PRIME → CUDA.  hw_device_ctx points to the CUDA device; FFmpeg
    // uses CUDA external memory import (cuImportExternalMemory) internally to
    // map the DMA-BUF without a GPU→CPU copy.  Requires NVIDIA driver 470+.
    let mut hwmap_ctx: *mut AVFilterContext = std::ptr::null_mut();
    let ret = avfilter_graph_create_filter(
        &mut hwmap_ctx,
        avfilter_get_by_name(c"hwmap".as_ptr()),
        c"hwmap".as_ptr(),
        c"mode=read".as_ptr(),
        std::ptr::null_mut(),
        graph,
    );
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_create_filter (hwmap) failed: {}", ret);
    }
    (*hwmap_ctx).hw_device_ctx = av_buffer_ref(cuda_device_ctx);

    // scale_cuda: CUDA BGRA → CUDA NV12 via CUDA NPP (no CPU copies).
    let mut scale_ctx: *mut AVFilterContext = std::ptr::null_mut();
    let ret = avfilter_graph_create_filter(
        &mut scale_ctx,
        avfilter_get_by_name(c"scale_cuda".as_ptr()),
        c"scale".as_ptr(),
        c"format=nv12".as_ptr(),
        std::ptr::null_mut(),
        graph,
    );
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_create_filter (scale_cuda) failed: {}", ret);
    }
    (*scale_ctx).hw_device_ctx = av_buffer_ref(cuda_device_ctx);

    // buffersink: receives the CUDA NV12 frames for the encoder.
    let mut sink_ctx: *mut AVFilterContext = std::ptr::null_mut();
    let ret = avfilter_graph_create_filter(
        &mut sink_ctx,
        avfilter_get_by_name(c"buffersink".as_ptr()),
        c"out".as_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        graph,
    );
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_create_filter (buffersink) failed: {}", ret);
    }

    // Link: src → hwmap → scale → sink
    let ret = avfilter_link(src_ctx, 0, hwmap_ctx, 0);
    if ret < 0 { avfilter_graph_free(&mut (graph as *mut _)); av_buffer_unref(&mut (drm_frames_ref as *mut _)); bail!("avfilter_link src→hwmap failed: {}", ret); }
    let ret = avfilter_link(hwmap_ctx, 0, scale_ctx, 0);
    if ret < 0 { avfilter_graph_free(&mut (graph as *mut _)); av_buffer_unref(&mut (drm_frames_ref as *mut _)); bail!("avfilter_link hwmap→scale failed: {}", ret); }
    let ret = avfilter_link(scale_ctx, 0, sink_ctx, 0);
    if ret < 0 { avfilter_graph_free(&mut (graph as *mut _)); av_buffer_unref(&mut (drm_frames_ref as *mut _)); bail!("avfilter_link scale→sink failed: {}", ret); }

    let ret = avfilter_graph_config(graph, std::ptr::null_mut());
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_config failed: {}", ret);
    }

    Ok((drm_frames_ref, graph, src_ctx, sink_ctx))
}
