//! VA-API hardware H.264 encoder using ffmpeg-sys-next.
//!
//! Dual pipeline:
//!   GPU (zero-copy): DMA-BUF (ARGB8888) → DRM_PRIME frame → hwmap → scale_vaapi (NV12) → h264_vaapi
//!   CPU (fallback):  RGBA buffer → sws_scale (NV12) → av_hwframe_transfer_data → h264_vaapi
// FFI with ffmpeg-sys-next uses many intentional numeric casts.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    hw_device_ctx: *mut AVBufferRef,      // VAAPI device (derived from DRM when possible)
    hw_frames_ctx: *mut AVBufferRef,      // VAAPI NV12 frames pool for the encoder
    sws_ctx: *mut SwsContext,             // RGBA→NV12 conversion for the CPU path

    // Zero-copy DMA-BUF path
    drm_device_ctx: *mut AVBufferRef,     // DRM device context
    drm_frames_ctx: *mut AVBufferRef,     // DRM hw_frames_ctx (ARGB8888/BGRA, no pool)
    filter_graph: *mut AVFilterGraph,
    filter_buffersrc: *mut AVFilterContext,
    filter_buffersink: *mut AVFilterContext,
    dmabuf_path_ok: bool,

    keyframe_requested: Arc<AtomicBool>,
    width: i32,
    height: i32,
    frame_index: i64,
}

// SAFETY: raw pointers are only accessed from the single encoder task thread.
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    pub fn new(config: &crate::encoder::EncoderConfig) -> Result<Self> {
        let render_node = config.render_node.as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/dev/dri/renderD128".to_owned());

        let w = config.width as i32;
        let h = config.height as i32;

        unsafe {
            let device_path = CString::new(render_node.as_str())
                .context("Invalid render node path")?;

            // ----------------------------------------------------------------
            // 1. DRM device (needed for zero-copy DMA-BUF import)
            // ----------------------------------------------------------------
            let mut drm_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
            let ret = av_hwdevice_ctx_create(
                &mut drm_device_ctx,
                AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
                device_path.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if ret < 0 {
                tracing::warn!("Failed to create DRM device context ({}), DMA-BUF zero-copy unavailable", ret);
            }

            // ----------------------------------------------------------------
            // 2. VAAPI device — derived from DRM so they share the same fd,
            //    enabling zero-copy surface import.
            // ----------------------------------------------------------------
            let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();
            let derived = if !drm_device_ctx.is_null() {
                let ret = av_hwdevice_ctx_create_derived(
                    &mut hw_device_ctx,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    drm_device_ctx,
                    0,
                );
                ret >= 0
            } else {
                false
            };

            if !derived {
                // Fall back to a standalone VAAPI device (CPU path still works)
                if !hw_device_ctx.is_null() {
                    av_buffer_unref(&mut hw_device_ctx);
                }
                let ret = av_hwdevice_ctx_create(
                    &mut hw_device_ctx,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    device_path.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                );
                if ret < 0 {
                    if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                    bail!("av_hwdevice_ctx_create (VAAPI) failed for {}: error {}", render_node, ret);
                }
                if !derived {
                    tracing::debug!("Using standalone VAAPI device (DRM derivation unavailable)");
                }
            } else {
                tracing::debug!("VAAPI device derived from DRM device");
            }

            // ----------------------------------------------------------------
            // 3. Find h264_vaapi encoder and allocate codec context
            // ----------------------------------------------------------------
            let codec = avcodec_find_encoder_by_name(c"h264_vaapi".as_ptr());
            if codec.is_null() {
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("h264_vaapi codec not found — is ffmpeg built with VA-API support?");
            }

            let codec_ctx = avcodec_alloc_context3(codec);
            if codec_ctx.is_null() {
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("avcodec_alloc_context3 failed");
            }

            (*codec_ctx).width = w;
            (*codec_ctx).height = h;
            (*codec_ctx).pix_fmt = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*codec_ctx).time_base = AVRational { num: 1, den: 1000 };
            (*codec_ctx).framerate = AVRational { num: config.fps as i32, den: 1 };
            (*codec_ctx).gop_size = (config.fps * 2.0) as i32;
            (*codec_ctx).max_b_frames = 0;
            (*codec_ctx).bit_rate = (config.bitrate_kbps as i64) * 1000;
            (*codec_ctx).hw_device_ctx = av_buffer_ref(hw_device_ctx);

            // ----------------------------------------------------------------
            // 4. Encoder hw_frames_ctx — NV12 VAAPI pool
            // ----------------------------------------------------------------
            let hw_frames_ref = av_hwframe_ctx_alloc(hw_device_ctx);
            if hw_frames_ref.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("av_hwframe_ctx_alloc failed");
            }
            {
                let fc = (*hw_frames_ref).data as *mut AVHWFramesContext;
                (*fc).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
                (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*fc).width = w;
                (*fc).height = h;
                (*fc).initial_pool_size = 8;
            }
            let ret = av_hwframe_ctx_init(hw_frames_ref);
            if ret < 0 {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("av_hwframe_ctx_init (encoder) failed: {}", ret);
            }
            (*codec_ctx).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
            let hw_frames_ctx = hw_frames_ref;

            // ----------------------------------------------------------------
            // 5. Open the encoder
            // ----------------------------------------------------------------
            (*codec_ctx).flags |= AV_CODEC_FLAG2_LOCAL_HEADER;
            let mut opts: *mut AVDictionary = std::ptr::null_mut();
            av_dict_set(&mut opts, c"profile".as_ptr(), c"high".as_ptr(), 0);
            av_dict_set(&mut opts, c"level".as_ptr(), c"4.1".as_ptr(), 0);
            av_dict_set(&mut opts, c"rc_mode".as_ptr(), c"CBR".as_ptr(), 0);
            let ret = avcodec_open2(codec_ctx, codec, &mut opts);
            if !opts.is_null() { av_dict_free(&mut opts); }
            if ret < 0 {
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("avcodec_open2 failed: {}", ret);
            }

            // ----------------------------------------------------------------
            // 6. sws_ctx — RGBA→NV12 for the CPU fallback path
            // ----------------------------------------------------------------
            let sws_ctx = sws_getContext(
                w, h, AVPixelFormat::AV_PIX_FMT_RGBA,
                w, h, AVPixelFormat::AV_PIX_FMT_NV12,
                SWS_BILINEAR,
                std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null(),
            );
            if sws_ctx.is_null() {
                avcodec_free_context(&mut (codec_ctx as *mut _));
                av_buffer_unref(&mut hw_device_ctx);
                if !drm_device_ctx.is_null() { av_buffer_unref(&mut drm_device_ctx); }
                bail!("sws_getContext failed");
            }

            // ----------------------------------------------------------------
            // 7. DRM hw_frames_ctx and filter graph for the DMA-BUF zero-copy path
            // ----------------------------------------------------------------
            let (drm_frames_ctx, filter_graph, filter_buffersrc, filter_buffersink, dmabuf_path_ok) =
                if !drm_device_ctx.is_null() {
                    match init_dmabuf_pipeline(drm_device_ctx, hw_device_ctx, w, h, config.fps) {
                        Ok((dfc, fg, src, sink)) => {
                            tracing::info!("VA-API zero-copy DMA-BUF pipeline ready");
                            (dfc, fg, src, sink, true)
                        }
                        Err(e) => {
                            tracing::warn!("DMA-BUF pipeline setup failed ({}); GPU frames will error", e);
                            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), false)
                        }
                    }
                } else {
                    (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), false)
                };

            tracing::info!("VA-API encoder initialised on {}", render_node);

            Ok(Self {
                codec_ctx,
                hw_device_ctx,
                hw_frames_ctx,
                sws_ctx,
                drm_device_ctx,
                drm_frames_ctx,
                filter_graph,
                filter_buffersrc,
                filter_buffersink,
                dmabuf_path_ok,
                keyframe_requested: Arc::new(AtomicBool::new(false)),
                width: w,
                height: h,
                frame_index: 0,
            })
        }
    }

    /// Convert an RGBA CPU buffer to a software NV12 AVFrame.
    unsafe fn rgba_to_nv12_frame(&mut self, rgba: &[u8], pts_ms: u64) -> Result<AvFramePtr> {
        let mut sw_frame = AvFramePtr::alloc()?;
        let f = sw_frame.as_mut();
        (*f).width = self.width;
        (*f).height = self.height;
        (*f).format = AVPixelFormat::AV_PIX_FMT_NV12 as i32;
        let ret = av_frame_get_buffer(f, 0);
        if ret < 0 { bail!("av_frame_get_buffer (sw) failed: {}", ret); }

        let src_data = [rgba.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null()];
        let src_linesize = [self.width * 4, 0, 0, 0];
        sws_scale(
            self.sws_ctx,
            src_data.as_ptr(), src_linesize.as_ptr(), 0, self.height,
            (*f).data.as_ptr() as *mut *mut u8,
            (*f).linesize.as_ptr() as *mut i32,
        );
        (*f).pts = pts_ms as i64;
        Ok(sw_frame)
    }

    /// Upload a software NV12 frame into a VAAPI hardware frame.
    unsafe fn upload_to_hw(&mut self, sw_frame: &AvFramePtr) -> Result<AvFramePtr> {
        let mut hw_frame = AvFramePtr::alloc()?;
        let ret = av_hwframe_get_buffer(self.hw_frames_ctx, hw_frame.as_mut(), 0);
        if ret < 0 { bail!("av_hwframe_get_buffer failed: {}", ret); }
        let ret = av_hwframe_transfer_data(hw_frame.as_mut(), sw_frame.as_ptr(), 0);
        if ret < 0 { bail!("av_hwframe_transfer_data failed: {}", ret); }
        (*hw_frame.as_mut()).pts = (*sw_frame.as_ptr()).pts;
        Ok(hw_frame)
    }

    /// Push a DMA-BUF through the filter graph (hwmap → scale_vaapi) to get
    /// a VAAPI NV12 frame, then encode it.  Zero GPU→CPU copies.
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

        // Hand the DRM_PRIME frame to the filter graph (hwmap + scale_vaapi).
        let ret = av_buffersrc_add_frame_flags(
            self.filter_buffersrc, f, AV_BUFFERSRC_FLAG_KEEP_REF as i32,
        );
        if ret < 0 { bail!("av_buffersrc_add_frame_flags failed: {}", ret); }

        // Receive the resulting VAAPI NV12 frame.
        let mut nv12_frame = AvFramePtr::alloc()?;
        let ret = av_buffersink_get_frame(self.filter_buffersink, nv12_frame.as_mut());
        if ret == AVERROR(EAGAIN) { return Ok(None); }
        if ret < 0 { bail!("av_buffersink_get_frame failed: {}", ret); }

        self.send_and_receive(&mut nv12_frame, frame.pts_ms)
    }

    /// Submit a hardware frame to the encoder and drain one packet.
    unsafe fn send_and_receive(
        &mut self,
        hw_frame: &mut AvFramePtr,
        fallback_pts_ms: u64,
    ) -> Result<Option<EncodedFrame>> {
        let ret = avcodec_send_frame(self.codec_ctx, hw_frame.as_mut());
        if ret < 0 && ret != AVERROR(EAGAIN) {
            bail!("avcodec_send_frame failed: {}", ret);
        }

        let mut pkt = AvPacketPtr::alloc()?;
        let ret = avcodec_receive_packet(self.codec_ctx, pkt.as_mut());
        if ret == AVERROR(EAGAIN) || ret == AVERROR_EOF { return Ok(None); }
        if ret < 0 { bail!("avcodec_receive_packet failed: {}", ret); }

        let p = &*pkt.as_mut();
        let data = std::slice::from_raw_parts(p.data, p.size as usize);
        let is_keyframe = (p.flags & AV_PKT_FLAG_KEY) != 0;
        let pts_ms = if p.pts != AV_NOPTS_VALUE { p.pts as u64 } else { fallback_pts_ms };
        self.frame_index += 1;

        Ok(Some(EncodedFrame {
            data: Bytes::copy_from_slice(data),
            pts_ms,
            is_keyframe,
        }))
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.filter_graph.is_null() { avfilter_graph_free(&mut self.filter_graph); }
            if !self.sws_ctx.is_null() { sws_freeContext(self.sws_ctx); }
            if !self.codec_ctx.is_null() { avcodec_free_context(&mut (self.codec_ctx as *mut _)); }
            if !self.hw_frames_ctx.is_null() { av_buffer_unref(&mut self.hw_frames_ctx); }
            if !self.drm_frames_ctx.is_null() { av_buffer_unref(&mut self.drm_frames_ctx); }
            if !self.hw_device_ctx.is_null() { av_buffer_unref(&mut self.hw_device_ctx); }
            if !self.drm_device_ctx.is_null() { av_buffer_unref(&mut self.drm_device_ctx); }
        }
    }
}

impl VideoEncoder for VaapiEncoder {
    fn encode(&mut self, frame: CapturedFrame) -> Result<Option<EncodedFrame>> {
        if self.keyframe_requested.swap(false, Ordering::Relaxed) {
            tracing::debug!("Keyframe requested (VA-API: will emit on next GOP boundary)");
        }

        unsafe {
            if frame.dmabuf.is_some() {
                if !self.dmabuf_path_ok {
                    bail!("VA-API DMA-BUF pipeline not available; cannot encode GPU frame");
                }
                self.encode_from_dmabuf(&frame)
            } else if let Some(ref rgba) = frame.rgba_buffer {
                let sw_frame = self.rgba_to_nv12_frame(rgba, frame.pts_ms)?;
                let mut hw_frame = self.upload_to_hw(&sw_frame)?;
                self.send_and_receive(&mut hw_frame, frame.pts_ms)
            } else {
                bail!("CapturedFrame has neither dmabuf nor rgba_buffer");
            }
        }
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    fn update_bitrate(&mut self, kbps: u32) {
        unsafe { (*self.codec_ctx).bit_rate = (kbps as i64) * 1000; }
    }
}

/// Free callback for the AVBufferRef wrapping an AVDRMFrameDescriptor.
unsafe extern "C" fn free_drm_desc(_opaque: *mut c_void, data: *mut u8) {
    av_free(data as *mut c_void);
}

/// Build the DRM hw_frames_ctx and the filter graph:
///   buffer (DRM_PRIME / BGRA)  →  hwmap (→ VAAPI)  →  scale_vaapi (NV12)  →  buffersink
///
/// Returns `(drm_frames_ctx, filter_graph, buffersrc_ctx, buffersink_ctx)`.
unsafe fn init_dmabuf_pipeline(
    drm_device_ctx: *mut AVBufferRef,
    vaapi_device_ctx: *mut AVBufferRef,
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

    // hwmap: DRM_PRIME → VAAPI.  hw_device_ctx tells it the target device.
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
    (*hwmap_ctx).hw_device_ctx = av_buffer_ref(vaapi_device_ctx);

    // scale_vaapi: VAAPI BGRA → VAAPI NV12 via VA-API VPP (GPU, no CPU copies).
    let mut scale_ctx: *mut AVFilterContext = std::ptr::null_mut();
    let ret = avfilter_graph_create_filter(
        &mut scale_ctx,
        avfilter_get_by_name(c"scale_vaapi".as_ptr()),
        c"scale".as_ptr(),
        c"format=nv12".as_ptr(),
        std::ptr::null_mut(),
        graph,
    );
    if ret < 0 {
        avfilter_graph_free(&mut (graph as *mut _));
        av_buffer_unref(&mut (drm_frames_ref as *mut _));
        bail!("avfilter_graph_create_filter (scale_vaapi) failed: {}", ret);
    }
    (*scale_ctx).hw_device_ctx = av_buffer_ref(vaapi_device_ctx);

    // buffersink: receives the VAAPI NV12 frames for the encoder.
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
