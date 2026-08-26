//! Input capture for the tuner: discover an audio input source (mic,
//! interface) and tap it into a rolling ring buffer.
//!
//! This module holds the shared cross-platform types (`CaptureNode`,
//! `CaptureSession`, `write_wav`, `wav_header_rate`) and the Linux PipeWire
//! implementation of `list_input_sources`/`start_capture`. The non-Linux
//! (cpal) implementation lives in `capture_cpal.rs` and is re-exported from
//! the bottom of this module.

use crate::buffer::{CHANNELS, SAMPLE_RATE};
use crate::ring::RollingRing;
#[cfg(target_os = "linux")]
use crate::stream_clock::ClockSnapshot;
use crate::stream_clock::StreamClock;
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::{properties::properties, spa};
#[cfg(target_os = "linux")]
use spa::pod::Pod;
#[cfg(target_os = "linux")]
use std::cell::RefCell;
#[cfg(target_os = "linux")]
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct CaptureNode {
    pub id: u32,
    /// object.serial — what `target.object` actually matches on modern
    /// PipeWire (registry ids do NOT work; targeting by id silently falls
    /// back to the default source).
    pub serial: u64,
    pub app: String,   // application.name or node.name fallback
    pub media: String, // media.name; typically empty for mic/interface sources
}

#[cfg(target_os = "linux")]
fn pw_err(e: pw::Error) -> crate::error::Error {
    std::io::Error::other(e.to_string()).into()
}

/// One-shot registry scan for capture sources (mics, audio interfaces:
/// media.class == "Audio/Source").
#[cfg(target_os = "linux")]
pub fn list_input_sources() -> crate::error::Result<Vec<CaptureNode>> {
    let handle = std::thread::Builder::new()
        .name("dredge-pw-scan-in".into())
        .spawn(scan_input_sources)?;
    handle
        .join()
        .map_err(|_| std::io::Error::other("pipewire scan thread panicked"))?
        .map_err(pw_err)
}

#[cfg(target_os = "linux")]
fn scan_input_sources() -> Result<Vec<CaptureNode>, pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let found: Rc<RefCell<Vec<CaptureNode>>> = Rc::new(RefCell::new(Vec::new()));
    let _listener = registry
        .add_listener_local()
        .global({
            let found = found.clone();
            move |global| {
                let Some(props) = global.props.as_ref() else {
                    return;
                };
                if props.get("media.class") != Some("Audio/Source") {
                    return;
                }
                // For physical sources application.name is usually empty, so
                // node.description (a friendly device name) is preferred.
                let app = props
                    .get("node.description")
                    .or_else(|| props.get("application.name"))
                    .or_else(|| props.get("node.name"))
                    .unwrap_or("")
                    .to_owned();
                let media = props.get("media.name").unwrap_or("").to_owned();
                let serial = props
                    .get("object.serial")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(u64::from(global.id));
                found.borrow_mut().push(CaptureNode {
                    id: global.id,
                    serial,
                    app,
                    media,
                });
            }
        })
        .register();

    let timer = mainloop.loop_().add_timer({
        let weak = mainloop.downgrade();
        move |_| {
            if let Some(ml) = weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer
        .update_timer(Some(Duration::from_millis(300)), None)
        .into_result()
        .map_err(pw::Error::SpaError)?;

    mainloop.run();
    drop(timer);
    Ok(found.take())
}

/// A live capture of one application node into a rolling ring buffer.
pub struct CaptureSession {
    pub ring: Arc<Mutex<RollingRing>>,
    pub node: CaptureNode,
    clock: Arc<StreamClock>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureSession {
    /// Assemble a session from a backend's ring/clock/stop/thread.
    /// Backend-agnostic.
    pub(crate) fn from_parts(
        ring: Arc<Mutex<RollingRing>>,
        node: CaptureNode,
        clock: Arc<StreamClock>,
        stop: Arc<AtomicBool>,
        thread: JoinHandle<()>,
    ) -> Self {
        Self {
            ring,
            node,
            clock,
            stop,
            thread: Some(thread),
        }
    }

    /// The capture stream's timing publisher. Arm it briefly around a recording
    /// to receive `ClockSnapshot`s; it does nothing on the audio path otherwise.
    pub fn clock(&self) -> Arc<StreamClock> {
        self.clock.clone()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join(); // stop-poll timer wakes the loop within 100 ms
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.shutdown(); // idempotent: thread already taken after stop()
    }
}

/// Tap an input source (`node.serial`) into a rolling ring of `buffer_secs`
/// seconds for the tuner.
#[cfg(target_os = "linux")]
pub fn start_capture(node: CaptureNode, buffer_secs: f64) -> crate::error::Result<CaptureSession> {
    let ring = Arc::new(Mutex::new(RollingRing::with_secs(buffer_secs)));
    let clock = Arc::new(StreamClock::default());
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let ring = ring.clone();
        let clock = clock.clone();
        let stop = stop.clone();
        let node = node.clone();
        std::thread::Builder::new()
            .name("dredge-pw-cap".into())
            .spawn(move || {
                if let Err(e) = run_capture(node, ring, clock, stop) {
                    eprintln!("dredge capture thread failed: {e}");
                }
            })?
    };
    Ok(CaptureSession::from_parts(ring, node, clock, stop, thread))
}

/// Tap an input by its opaque device id (the `AudioDevice.id` from
/// `device::list_input_devices`). On Linux that id is the `object.serial`
/// string, so this targets `target.object` directly without a registry scan.
#[cfg(target_os = "linux")]
pub fn start_capture_by_id(id: &str, buffer_secs: f64) -> crate::error::Result<CaptureSession> {
    // The id must be a numeric object.serial; a non-numeric id (e.g. a stale
    // setting from the old name-based scheme) is an error rather than a silent
    // serial 0, which could target the wrong node or PipeWire's default.
    let serial: u64 = id
        .parse()
        .map_err(|_| crate::error::Error::Audio(format!("invalid input device id: {id:?}")))?;
    let node = CaptureNode {
        id: 0,
        serial,
        app: String::new(),
        media: String::new(),
    };
    start_capture(node, buffer_secs)
}

#[cfg(target_os = "linux")]
struct CapState {
    ring: Arc<Mutex<RollingRing>>,
    clock: Arc<StreamClock>,
    scratch: Vec<f32>,
    /// One-shot guard so the `DREDGE_DEBUG` raw-`pw_time` dump prints once per
    /// session, not every RT cycle.
    debug_printed: bool,
}

#[cfg(target_os = "linux")]
fn run_capture(
    node: CaptureNode,
    ring: Arc<Mutex<RollingRing>>,
    clock: Arc<StreamClock>,
    stop: Arc<AtomicBool>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::AUDIO_CHANNELS => "2",
        *pw::keys::NODE_NAME => "dredge-capture",
    };
    // Target the chosen application output stream node directly; PipeWire
    // links us to its monitor ports. target.object matches object.serial
    // (or node.name) — never the registry id.
    props.insert(*pw::keys::TARGET_OBJECT, node.serial.to_string());

    let stream = pw::stream::StreamBox::new(&core, "dredge-capture", props)?;

    let state = CapState {
        ring,
        clock,
        scratch: Vec::with_capacity(8192 * CHANNELS),
        debug_printed: false,
    };

    let _listener = stream
        .add_local_listener_with_user_data(state)
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(slice) = data.data() else {
                return;
            };
            let end = (offset + size).min(slice.len());
            let bytes = &slice[offset.min(end)..end];
            state.scratch.clear();
            state.scratch.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
            // This runs on PipeWire's RT thread (RT_PROCESS). Never *block* on
            // the lock — a blocking acquire here risks priority inversion / an
            // xrun if the control thread is mid-snapshot. try_lock and drop this
            // buffer on the rare contention instead (the control thread only
            // holds the lock briefly at grab time, and a dropped buffer during a
            // grab is samples past the grab point anyway).
            let ring_total = if let Ok(mut ring) = state.ring.try_lock() {
                ring.push(&state.scratch);
                Some(ring.total_frames_written() as i64)
            } else {
                None
            };

            // Publish a timing snapshot so the control thread can map graph time
            // to a ring frame. `StreamClock::store` is a no-op (and allocates
            // nothing) unless the control thread has armed it around a recording,
            // so the steady tuner path pays only the `pw_stream_get_time_n` read.
            if let (Some(ring_total), Ok(t)) = (ring_total, stream.time()) {
                let raw = t.as_raw();
                // One-shot raw dump so a human can confirm the field semantics on
                // real hardware (frames/sec = denom/num; delay is in ticks).
                if !state.debug_printed && std::env::var("DREDGE_DEBUG").is_ok() {
                    eprintln!(
                        "dredge capture pw_time: now={} rate.num={} rate.denom={} ticks={} delay={}",
                        raw.now, raw.rate.num, raw.rate.denom, raw.ticks, raw.delay
                    );
                    state.debug_printed = true;
                }
                // rate is seconds-per-tick (num/denom), so frames/sec = denom/num.
                // Guard a zero numerator (uninitialized/invalid) — skip this cycle.
                if raw.rate.num != 0 {
                    let rate_hz = i64::from(raw.rate.denom) / i64::from(raw.rate.num);
                    let snap = ClockSnapshot {
                        now_ns: t.now(),
                        ticks: raw.ticks as i64,
                        rate_hz,
                    };
                    state.clock.store(snap, ring_total, raw.delay);
                }
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // poll the stop flag; quit the loop when the session is dropped/stopped
    let timer = mainloop.loop_().add_timer({
        let weak = mainloop.downgrade();
        move |_| {
            if stop.load(Ordering::Relaxed) {
                if let Some(ml) = weak.upgrade() {
                    ml.quit();
                }
            }
        }
    });
    timer
        .update_timer(
            Some(Duration::from_millis(100)),
            Some(Duration::from_millis(100)),
        )
        .into_result()
        .map_err(pw::Error::SpaError)?;

    mainloop.run();
    drop(timer);
    Ok(())
}

/// Sample rate from a WAV file's header — cheap (no decode).
pub fn wav_header_rate(path: &std::path::Path) -> crate::error::Result<u32> {
    Ok(hound::WavReader::open(path)
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .spec()
        .sample_rate)
}

/// Write interleaved stereo f32 to a 16-bit WAV at 48 kHz. Returns Ok(()).
pub fn write_wav(path: &std::path::Path, interleaved: &[f32]) -> crate::error::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec {
        channels: CHANNELS as u16,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w =
        hound::WavWriter::create(path, spec).map_err(|e| std::io::Error::other(e.to_string()))?;
    for s in interleaved {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        w.write_sample(v)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    w.finalize()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub use crate::capture_cpal::{list_input_sources, start_capture, start_capture_by_id};
