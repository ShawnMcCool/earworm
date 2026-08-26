//! Overdub recording orchestration. Pure helpers (span resolution, calibration
//! click detection) are unit-tested here; device capture lives behind the
//! `RecordingControl` trait so the dispatcher can be tested with a fake.

use engine::buffer::{CHANNELS, SAMPLE_RATE};
use engine::stream_clock::ClockSnapshot;

/// Which region a recording pass covers, chosen by the user at record time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Span {
    Song,
    Selection { start: f64, end: f64 },
    Loop { start: f64, end: f64 },
}

/// Resolve a span to a `[start_frame, end_frame)` source-frame window, clamped
/// to the song. Returns `None` if the window is empty.
pub fn resolve_span(span: Span, song_frames: i64) -> Option<(i64, i64)> {
    let to_frame = |s: f64| (s.max(0.0) * SAMPLE_RATE as f64).round() as i64;
    let (start, end) = match span {
        Span::Song => (0, song_frames),
        Span::Selection { start, end } | Span::Loop { start, end } => {
            (to_frame(start), to_frame(end))
        }
    };
    let start = start.clamp(0, song_frames);
    let end = end.clamp(0, song_frames);
    if end > start {
        Some((start, end))
    } else {
        None
    }
}

/// Find the first frame whose absolute sample exceeds `threshold` in an
/// interleaved stereo recording. Used by latency calibration: emit a click at
/// recording frame 0, and the detected onset is the round-trip latency.
pub fn detect_click_onset(interleaved: &[f32], threshold: f32) -> Option<usize> {
    interleaved
        .as_chunks::<CHANNELS>()
        .0
        .iter()
        .position(|f| f.iter().any(|s| s.abs() > threshold))
}

/// Downsample the first `window_frames` of an interleaved-stereo window to
/// `points` peak-amplitude buckets — each bucket is the max abs sample over its
/// frames (both channels). Drives the loopback-calibration envelope the UI
/// draws. Buckets past the end of `interleaved` read as 0.
pub fn peak_envelope(interleaved: &[f32], window_frames: usize, points: usize) -> Vec<f32> {
    if points == 0 {
        return Vec::new();
    }
    let frames_per_bucket = (window_frames / points).max(1);
    let total_frames = interleaved.len() / CHANNELS;
    (0..points)
        .map(|b| {
            let start = b * frames_per_bucket;
            let end = (start + frames_per_bucket).min(total_frames);
            interleaved
                .get(start * CHANNELS..end * CHANNELS)
                .unwrap_or(&[])
                .iter()
                .fold(0.0f32, |m, s| m.max(s.abs()))
        })
        .collect()
}

/// Capture backend. The real implementation taps a PipeWire/cpal input; the
/// fake returns canned audio so the dispatcher is testable.
pub trait RecordingControl: Send {
    /// Begin capturing from `device_id`, sizing the buffer for `len_frames`.
    fn start(&mut self, device_id: &str, len_frames: i64) -> Result<(), String>;
    /// Stop and return the captured interleaved-stereo f32 (up to `len_frames`).
    fn stop(&mut self) -> Result<Vec<f32>, String>;
    /// Arm the capture stream clock so it begins publishing timing snapshots.
    fn arm_clock(&self);
    /// Latest capture clock snapshot paired with the ring's
    /// `total_frames_written` sampled at the same instant, or `None` if nothing
    /// has been published yet.
    fn capture_snapshot(&self) -> Option<(ClockSnapshot, i64)>;
    /// Interleaved-stereo samples for the absolute ring-frame range
    /// `[ring_start, ring_start + len)`, or `None` if `ring_start` is negative or
    /// the range has been evicted from the ring.
    fn extract_range(&self, ring_start: i64, len: i64) -> Option<Vec<f32>>;
    /// Input-stream buffering reported by PipeWire (`pw_time.delay`), in frames.
    /// Half of the round-trip latency; the output stream is the other half.
    fn input_delay_frames(&self) -> i64;
    /// Stop the capture clock publishing.
    fn disarm_clock(&self);
    /// Start + arm a capture session on `device_id` for loopback latency
    /// calibration. The caller then emits an impulse out the output, waits for
    /// the cable round-trip, and reads the ring via `capture_snapshot` +
    /// `extract_range`, finishing with `stop`. Non-blocking (the wait is the
    /// caller's).
    fn calibrate_session(&mut self, device_id: &str) -> Result<(), String>;
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeRecorder {
    pub canned: Vec<f32>,
    pub started: Option<(String, i64)>,
    pub stopped: bool,
    /// Canned input-stream delay (frames) returned by `input_delay_frames`.
    pub input_delay: i64,
}

#[cfg(test)]
impl RecordingControl for FakeRecorder {
    fn start(&mut self, device_id: &str, len_frames: i64) -> Result<(), String> {
        self.started = Some((device_id.to_string(), len_frames));
        Ok(())
    }
    fn stop(&mut self) -> Result<Vec<f32>, String> {
        self.stopped = true;
        Ok(self.canned.clone())
    }
    fn calibrate_session(&mut self, device_id: &str) -> Result<(), String> {
        self.started = Some((device_id.to_string(), 0));
        Ok(())
    }
    fn arm_clock(&self) {}
    fn capture_snapshot(&self) -> Option<(ClockSnapshot, i64)> {
        Some((
            ClockSnapshot {
                now_ns: 0,
                ticks: 0,
                rate_hz: 48_000,
            },
            0,
        ))
    }
    fn extract_range(&self, _ring_start: i64, _len: i64) -> Option<Vec<f32>> {
        Some(self.canned.clone())
    }
    fn input_delay_frames(&self) -> i64 {
        self.input_delay
    }
    fn disarm_clock(&self) {}
}

#[derive(Default)]
pub struct RealRecorder {
    capture: Option<engine::capture::CaptureSession>,
    len_frames: i64,
}

impl RecordingControl for RealRecorder {
    fn start(&mut self, device_id: &str, len_frames: i64) -> Result<(), String> {
        let secs = (len_frames as f64 / SAMPLE_RATE as f64) + 1.0; // +1s margin
        let cap =
            engine::capture::start_capture_by_id(device_id, secs).map_err(|e| e.to_string())?;
        self.capture = Some(cap);
        self.len_frames = len_frames;
        Ok(())
    }

    fn stop(&mut self) -> Result<Vec<f32>, String> {
        let cap = self.capture.take().ok_or("not recording")?;
        let secs = self.len_frames as f64 / SAMPLE_RATE as f64;
        let snap = cap
            .ring
            .lock()
            .map_err(|_| "capture ring poisoned")?
            .snapshot_last(secs);
        // the snapshot's MutexGuard is a temporary that drops at the end of the
        // previous statement, so the lock is released before cap.stop() joins
        // the capture thread (no deadlock).
        cap.stop();
        Ok(snap)
    }

    fn arm_clock(&self) {
        if let Some(cap) = &self.capture {
            cap.clock().arm();
        }
    }

    fn capture_snapshot(&self) -> Option<(ClockSnapshot, i64)> {
        let clock = self.capture.as_ref()?.clock();
        let snap = clock.load()?;
        Some((snap, clock.ring_total_at_snapshot()))
    }

    fn extract_range(&self, ring_start: i64, len: i64) -> Option<Vec<f32>> {
        if ring_start < 0 {
            return None;
        }
        let cap = self.capture.as_ref()?;
        let ring = cap.ring.lock().ok()?;
        ring.read_range(ring_start as u64, (ring_start + len) as u64)
    }

    fn input_delay_frames(&self) -> i64 {
        self.capture
            .as_ref()
            .map(|c| c.clock().delay_frames())
            .unwrap_or(0)
    }

    fn disarm_clock(&self) {
        if let Some(cap) = &self.capture {
            cap.clock().disarm();
        }
    }

    fn calibrate_session(&mut self, device_id: &str) -> Result<(), String> {
        // Roomy buffer for the loopback: the caller waits ~1s and reads a 1s
        // window from the impulse's emit frame, so 2.5s covers ring eviction.
        let secs = 2.5;
        let cap =
            engine::capture::start_capture_by_id(device_id, secs).map_err(|e| e.to_string())?;
        self.capture = Some(cap);
        self.len_frames = (secs * SAMPLE_RATE as f64) as i64;
        // Arm the capture clock so it publishes timing snapshots; the impulse's
        // graph-clock emit time is mapped to a ring frame through them.
        self.arm_clock();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_song_span_is_whole_song() {
        assert_eq!(resolve_span(Span::Song, 1000), Some((0, 1000)));
    }

    #[test]
    fn resolve_selection_converts_seconds_and_clamps() {
        let s = Span::Selection {
            start: 1.0,
            end: 2.0,
        };
        assert_eq!(
            resolve_span(s, 10 * SAMPLE_RATE as i64),
            Some((SAMPLE_RATE as i64, 2 * SAMPLE_RATE as i64))
        );
    }

    #[test]
    fn resolve_empty_span_is_none() {
        let s = Span::Selection {
            start: 2.0,
            end: 2.0,
        };
        assert_eq!(resolve_span(s, 10 * SAMPLE_RATE as i64), None);
    }

    #[test]
    fn detect_onset_finds_the_click() {
        let mut buf = vec![0.0f32; 50 * CHANNELS];
        buf.extend_from_slice(&[0.9, 0.9]);
        assert_eq!(detect_click_onset(&buf, 0.5), Some(50));
    }

    #[test]
    fn detect_onset_none_when_below_threshold() {
        let buf = vec![0.1f32; 100 * CHANNELS];
        assert_eq!(detect_click_onset(&buf, 0.5), None);
    }

    #[test]
    fn peak_envelope_buckets_max_abs_and_places_the_onset() {
        // 240 frames of silence, then a loud sample, then quiet tail.
        let mut buf = vec![0.0f32; 240 * CHANNELS];
        buf.extend_from_slice(&[-0.8, 0.8]);
        buf.extend(std::iter::repeat_n(0.0f32, 100 * CHANNELS));
        // 7200-frame window, 240 points -> 30 frames/bucket; onset at frame 240
        // lands in bucket 8.
        let env = peak_envelope(&buf, 7200, 240);
        assert_eq!(env.len(), 240);
        assert!((env[8] - 0.8).abs() < 1e-6, "onset bucket holds the peak");
        assert_eq!(env[0], 0.0, "emit bucket is silent");
        assert_eq!(env[239], 0.0, "buckets past the data read as 0");
    }

    #[test]
    fn peak_envelope_zero_points_is_empty() {
        assert!(peak_envelope(&[0.5, 0.5], 7200, 0).is_empty());
    }
}
