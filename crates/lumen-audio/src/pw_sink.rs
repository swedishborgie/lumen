//! Native PipeWire virtual audio sink.
//!
//! Creates a PipeWire stream that appears as an `Audio/Sink` in the PipeWire
//! graph.  Audio routed to this sink by other applications is delivered
//! directly to our [`process`] callback as F32LE interleaved samples, which
//! are forwarded to the Opus encoder via a `std::sync::mpsc` channel.
//!
//! The default audio sink is switched to (and from) this virtual sink via the
//! PipeWire metadata API whenever the connected peer count crosses the 0 ↔ 1
//! boundary.  On drop the original default is restored and the stream is
//! destroyed.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use anyhow::{Context, Result};
use pipewire as pw;
use pipewire::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::param::ParamType;
use spa::pod::Pod;
use spa::utils::Direction;

/// A single frame of raw PCM audio delivered by PipeWire.
///
/// Samples are interleaved F32LE at the sample rate and channel count
/// configured in [`AudioConfig`](crate::AudioConfig).
pub struct PcmFrame {
    pub samples: Vec<f32>,
}

/// Control messages sent from the Tokio world to the PipeWire thread.
#[derive(Debug)]
pub enum ControlMsg {
    /// Set Lumen's virtual sink as the system default audio output.
    Activate,
    /// Restore the original default audio output.
    Deactivate,
    /// Quit the PipeWire main loop; called by [`PipeWireSink::drop`].
    Stop,
}

/// Owns a PipeWire thread that runs a virtual audio sink stream.
///
/// On drop the original default audio sink is restored (best-effort) and
/// the PipeWire thread is joined.
pub struct PipeWireSink {
    thread: Option<std::thread::JoinHandle<()>>,
    /// Sends control messages to the PipeWire thread.
    pub control_tx: pw::channel::Sender<ControlMsg>,
    /// Receives decoded PCM frames produced by the PipeWire `process` callback.
    pub pcm_rx: mpsc::Receiver<PcmFrame>,
}

impl PipeWireSink {
    /// Spawn a PipeWire thread and connect a virtual `Audio/Sink` stream.
    ///
    /// Returns once the thread is running and the stream is connecting.
    /// PCM frames arrive on [`pcm_rx`]; use [`control_tx`] to activate or
    /// deactivate the default-sink override.
    ///
    /// [`pcm_rx`]: Self::pcm_rx
    /// [`control_tx`]: Self::control_tx
    pub fn create(sample_rate: u32, channels: u8) -> Result<Self> {
        let (pcm_tx, pcm_rx) = mpsc::sync_channel::<PcmFrame>(8);
        let (ctrl_tx, ctrl_rx) = pw::channel::channel::<ControlMsg>();

        // Synchronise the caller: block until the PW thread has finished its
        // initial setup before returning.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        let thread = std::thread::Builder::new()
            .name("lumen-pipewire".into())
            .spawn(move || {
                if let Err(e) =
                    run_pw_thread(sample_rate, channels, pcm_tx, ctrl_rx, ready_tx)
                {
                    tracing::error!("PipeWire thread exited with error: {e:#}");
                }
            })
            .context("Failed to spawn PipeWire thread")?;

        // Propagate any startup error from the PW thread.
        ready_rx
            .recv()
            .context("PipeWire thread dropped ready channel unexpectedly")?
            .context("PipeWire thread failed to initialise")?;

        Ok(Self { thread: Some(thread), control_tx: ctrl_tx, pcm_rx })
    }

    /// Signal the PipeWire thread to set this sink as the system default.
    pub fn activate(&self) {
        if self.control_tx.send(ControlMsg::Activate).is_err() {
            tracing::warn!("PipeWireSink::activate: control channel closed");
        }
    }

    /// Signal the PipeWire thread to restore the original default sink.
    pub fn deactivate(&self) {
        if self.control_tx.send(ControlMsg::Deactivate).is_err() {
            tracing::warn!("PipeWireSink::deactivate: control channel closed");
        }
    }
}

impl Drop for PipeWireSink {
    fn drop(&mut self) {
        // Best-effort restore then stop.
        self.deactivate();
        let _ = self.control_tx.send(ControlMsg::Stop);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

// ── PipeWire thread ───────────────────────────────────────────────────────────

fn run_pw_thread(
    sample_rate: u32,
    channels: u8,
    pcm_tx: mpsc::SyncSender<PcmFrame>,
    ctrl_rx: pw::channel::Receiver<ControlMsg>,
    ready_tx: mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .context("Failed to create PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .context("Failed to create PipeWire context")?;
    let core = context
        .connect_rc(None)
        .context("Failed to connect to PipeWire daemon")?;
    let registry = core
        .get_registry_rc()
        .context("Failed to get PipeWire registry")?;

    // ── Metadata tracking ─────────────────────────────────────────────────
    //
    // We bind the "default" metadata object and use it to read/write
    // `default.audio.sink`.  The original value is captured from the first
    // `property` event so we can restore it on deactivate.

    let meta: Rc<RefCell<Option<pw::metadata::Metadata>>> = Rc::new(RefCell::new(None));
    let meta_listener: Rc<RefCell<Option<pw::metadata::MetadataListener>>> =
        Rc::new(RefCell::new(None));
    let original_default: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let meta_for_ctrl = meta.clone();
    let orig_for_ctrl = original_default.clone();

    let _reg_listener = {
        let meta = meta.clone();
        let meta_listener = meta_listener.clone();
        let original_default = original_default.clone();
        let registry_for_bind = registry.clone();

        registry
            .add_listener_local()
            .global(move |global| {
                if global.type_ != pw::types::ObjectType::Metadata {
                    return;
                }
                if meta.borrow().is_some() {
                    return; // already bound
                }

                let meta_obj: pw::metadata::Metadata = match registry_for_bind.bind(global) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("Failed to bind PipeWire metadata object: {e}");
                        return;
                    }
                };

                // Listen for property events to capture the original default.
                let orig = original_default.clone();
                let listener = meta_obj
                    .add_listener_local()
                    .property(move |subject, key, _type_, value| {
                        if subject == 0
                            && key == Some("default.audio.sink")
                            && orig.borrow().is_none()
                        {
                            let saved = value.map(str::to_owned);
                            tracing::debug!(saved = ?saved, "Saved original default audio sink");
                            *orig.borrow_mut() = saved;
                        }
                        0
                    })
                    .register();

                *meta_listener.borrow_mut() = Some(listener);
                *meta.borrow_mut() = Some(meta_obj);
            })
            .register()
    };

    // ── Stream setup ──────────────────────────────────────────────────────

    let stream_props = pw::properties::properties! {
        *pw::keys::MEDIA_CLASS      => "Audio/Sink",
        *pw::keys::NODE_NAME        => "lumen_capture",
        *pw::keys::NODE_DESCRIPTION => "Lumen Audio Capture",
        *pw::keys::MEDIA_TYPE       => "Audio",
        *pw::keys::MEDIA_CATEGORY   => "Capture",
    };

    let stream = pw::stream::StreamRc::new(core, "lumen-capture", stream_props)
        .context("Failed to create PipeWire stream")?;

    let _stream_listener = stream
        .add_local_listener_with_user_data(pcm_tx)
        .state_changed(|_stream, _data, old, new| {
            tracing::debug!("PipeWire stream state: {old:?} → {new:?}");
        })
        .param_changed(|_stream, _data, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                tracing::info!(
                    rate = info.rate(),
                    channels = info.channels(),
                    "PipeWire stream format negotiated"
                );
            }
        })
        .process(|stream, pcm_tx| {
            let Some(mut buf) = stream.dequeue_buffer() else { return };
            let datas = buf.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let n_bytes = data.chunk().size() as usize;
            if n_bytes == 0 {
                return;
            }
            let Some(raw) = data.data() else { return };
            let n_samples = n_bytes / std::mem::size_of::<f32>();
            let mut samples = Vec::with_capacity(n_samples);
            for chunk in raw[..n_bytes].chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            if pcm_tx.try_send(PcmFrame { samples }).is_err() {
                tracing::trace!("PipeWireSink: PCM frame dropped (encoder lagging)");
            }
        })
        .register();

    // Build the SPA format pod.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(sample_rate);
    audio_info.set_channels(channels as u32);

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id:    ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let pod_bytes: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .context("Failed to serialise SPA audio format pod")?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&pod_bytes)
        .context("Failed to build SPA Pod from bytes")?];

    stream
        .connect(
            Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("Failed to connect PipeWire stream")?;

    tracing::info!("PipeWire virtual sink stream connecting");

    // ── Control-message handler ────────────────────────────────────────────

    let mainloop_for_stop = mainloop.clone();

    let _ctrl_source = ctrl_rx.attach(mainloop.loop_(), move |msg| match msg {
        ControlMsg::Activate => {
            if let Some(meta) = meta_for_ctrl.borrow().as_ref() {
                tracing::info!("Setting default audio sink → lumen_capture");
                meta.set_property(
                    0,
                    "default.audio.sink",
                    Some("Spa:String:JSON"),
                    Some("{ \"name\": \"lumen_capture\" }"),
                );
            } else {
                tracing::warn!(
                    "Cannot activate: PipeWire metadata object not yet found"
                );
            }
        }
        ControlMsg::Deactivate => {
            if let Some(meta) = meta_for_ctrl.borrow().as_ref() {
                let orig = orig_for_ctrl.borrow();
                if let Some(ref original) = *orig {
                    tracing::info!("Restoring default audio sink → {original}");
                    meta.set_property(
                        0,
                        "default.audio.sink",
                        Some("Spa:String:JSON"),
                        Some(original),
                    );
                } else {
                    tracing::warn!(
                        "Cannot restore default audio sink: original value not captured"
                    );
                }
            }
        }
        ControlMsg::Stop => {
            mainloop_for_stop.quit();
        }
    });

    // Notify the caller that setup is complete.
    let _ = ready_tx.send(Ok(()));

    // Block until Stop is received.
    mainloop.run();

    tracing::info!("PipeWire thread exiting");

    // pw::deinit() is intentionally omitted: it is not safe to call when other
    // PipeWire objects may still be alive on the stack during unwind, and the
    // process-level cleanup is acceptable for a long-lived server.

    Ok(())
}
