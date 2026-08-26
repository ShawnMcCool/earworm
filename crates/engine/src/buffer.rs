pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

/// Whole song in memory: interleaved stereo f32 at 48 kHz.
#[derive(Debug, Clone, PartialEq)]
pub struct SongBuffer {
    pub data: Vec<f32>,
}

impl SongBuffer {
    pub fn frames(&self) -> usize {
        self.data.len() / CHANNELS
    }
    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / SAMPLE_RATE as f64
    }
}

/// Frames over which a stem gain slews to a new target — 10 ms at 48 kHz. Short
/// enough to feel instant, long enough to kill the zipper noise of a step change
/// (e.g. a routine block muting a stem between loop passes).
pub const GAIN_RAMP_FRAMES: usize = 480;

/// One or more equal-length stems mixed with per-stem gains. Gains are *slewed*:
/// `set_gain` moves a stem's target, and the applied `gains` chase it one frame
/// at a time (see `step_gains`), so changes are click-free. `settle` snaps the
/// applied gains to target for callers that bake a static mix (export).
#[derive(Debug, Clone)]
pub struct StemSet {
    pub stems: Vec<std::sync::Arc<SongBuffer>>, // len >= 1; equal frames (pad on construction)
    pub gains: Vec<f32>,                        // applied (current), same len, 0.0..=1.5
    pub target_gains: Vec<f32>,                 // commanded; `gains` slews toward this
}

impl StemSet {
    pub fn single(buf: SongBuffer) -> Self {
        Self {
            stems: vec![std::sync::Arc::new(buf)],
            gains: vec![1.0],
            target_gains: vec![1.0],
        }
    }

    /// Pads shorter stems with silence to the longest.
    pub fn new(stems: Vec<SongBuffer>) -> Self {
        let max = stems.iter().map(SongBuffer::frames).max().unwrap_or(0);
        let stems: Vec<_> = stems
            .into_iter()
            .map(|mut s| {
                s.data.resize(max * CHANNELS, 0.0);
                std::sync::Arc::new(s)
            })
            .collect();
        let n = stems.len();
        Self {
            stems,
            gains: vec![1.0; n],
            target_gains: vec![1.0; n],
        }
    }

    pub fn frames(&self) -> usize {
        self.stems.first().map(|s| s.frames()).unwrap_or(0)
    }

    /// Set a stem's target gain (clamped to 0.0..=1.5); out-of-range stems are
    /// ignored. The applied gain slews toward it over `GAIN_RAMP_FRAMES`.
    pub fn set_gain(&mut self, idx: usize, gain: f32) {
        if let Some(t) = self.target_gains.get_mut(idx) {
            *t = gain.clamp(0.0, 1.5);
        }
    }

    /// Snap applied gains to their targets — for static-mix callers (export)
    /// that must not hear the slew.
    pub fn settle(&mut self) {
        self.gains.copy_from_slice(&self.target_gains);
    }

    /// True when every applied gain already equals its target (steady state).
    #[inline]
    pub fn gains_settled(&self) -> bool {
        self.gains == self.target_gains
    }

    /// Advance the applied gains one frame toward their targets.
    #[inline]
    pub fn step_gains(&mut self) {
        const STEP: f32 = 1.0 / GAIN_RAMP_FRAMES as f32;
        for (g, &t) in self.gains.iter_mut().zip(&self.target_gains) {
            let d = t - *g;
            if d.abs() <= STEP {
                *g = t;
            } else {
                *g += STEP * d.signum();
            }
        }
    }

    /// Mixed (left, right) at frame `pos` with the currently-applied gains. Does
    /// not advance the slew — callers reading two source positions per output
    /// frame (the crossfade) `step_gains` once per output frame themselves.
    #[inline]
    pub fn frame(&self, pos: usize) -> (f32, f32) {
        let i = pos * CHANNELS;
        let (mut l, mut r) = (0.0, 0.0);
        for (stem, gain) in self.stems.iter().zip(&self.gains) {
            l += stem.data[i] * gain;
            r += stem.data[i + 1] * gain;
        }
        (l, r)
    }

    /// Mix `out.len() / CHANNELS` contiguous frames starting at source frame
    /// `start` into `out` (overwrites), advancing the gain slew per frame. In
    /// steady state (gains settled) this is the autovectorized constant-gain
    /// fast path; only while a gain is slewing does it fall to the per-frame
    /// loop. `start` and the run length must stay within `frames()`.
    pub fn mix_into(&mut self, start: usize, out: &mut [f32]) {
        out.fill(0.0);
        if self.gains_settled() {
            let base = start * CHANNELS;
            for (stem, &gain) in self.stems.iter().zip(&self.gains) {
                let src = &stem.data[base..base + out.len()];
                for (o, s) in out.iter_mut().zip(src) {
                    *o += s * gain;
                }
            }
            return;
        }
        let frames = out.len() / CHANNELS;
        for f in 0..frames {
            self.step_gains();
            let i = (start + f) * CHANNELS;
            let (mut l, mut r) = (0.0, 0.0);
            for (stem, &g) in self.stems.iter().zip(&self.gains) {
                l += stem.data[i] * g;
                r += stem.data[i + 1] * g;
            }
            out[f * CHANNELS] = l;
            out[f * CHANNELS + 1] = r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(frames: usize, v: f32) -> SongBuffer {
        SongBuffer {
            data: vec![v; frames * CHANNELS],
        }
    }

    #[test]
    fn new_pads_shorter_stems_to_longest() {
        let set = StemSet::new(vec![constant(100, 0.25), constant(40, 0.5)]);
        assert_eq!(set.frames(), 100);
        // beyond the short stem's original end only the long stem contributes
        let (l, r) = set.frame(50);
        assert_eq!((l, r), (0.25, 0.25));
        // padded region of the short stem reads 0.0 (in bounds, silent)
        let short = &set.stems[1];
        assert_eq!(short.frames(), 100);
        assert_eq!(short.data[50 * CHANNELS], 0.0);
    }

    #[test]
    fn frame_sums_stems_with_gains() {
        let mut set = StemSet::new(vec![constant(10, 0.25), constant(10, 0.5)]);
        set.set_gain(1, 0.5);
        set.settle();
        let (l, r) = set.frame(3);
        assert!((l - 0.5).abs() < 1e-6, "l = {l}");
        assert!((r - 0.5).abs() < 1e-6, "r = {r}");
    }

    #[test]
    fn mix_into_matches_per_frame_mix() {
        let mut set = StemSet::new(vec![
            SongBuffer {
                data: (0..20).map(|i| i as f32).collect(), // 10 frames
            },
            SongBuffer {
                data: (0..20).map(|i| (i * 2) as f32).collect(),
            },
        ]);
        set.set_gain(1, 0.5);
        set.settle();
        let mut out = vec![0.0f32; 6 * CHANNELS];
        set.mix_into(2, &mut out); // frames 2..8
        for (f, frame) in out.as_chunks::<CHANNELS>().0.iter().enumerate() {
            let (l, r) = set.frame(2 + f);
            assert!((frame[0] - l).abs() < 1e-6 && (frame[1] - r).abs() < 1e-6);
        }
    }

    #[test]
    fn single_wraps_with_unity_gain() {
        let set = StemSet::single(constant(8, 0.3));
        assert_eq!(set.stems.len(), 1);
        assert_eq!(set.gains, vec![1.0]);
        assert_eq!(set.frames(), 8);
        assert_eq!(set.frame(0), (0.3, 0.3));
    }

    #[test]
    fn gain_slews_to_target_without_instant_jump() {
        // Drop a unity stem to silence: the applied gain must ramp, not snap.
        let mut set = StemSet::single(constant(GAIN_RAMP_FRAMES + 100, 1.0));
        set.set_gain(0, 0.0);
        assert!(!set.gains_settled(), "target set, not yet reached");

        let mut out = vec![0.0f32; (GAIN_RAMP_FRAMES + 50) * CHANNELS];
        set.mix_into(0, &mut out);

        // First frame stepped once → still essentially full, NOT zero.
        assert!(out[0] > 0.99, "first frame jumped to {}", out[0]);
        // Monotonically non-increasing across the ramp (no zipper / overshoot).
        for w in out.as_chunks::<CHANNELS>().0.windows(2) {
            assert!(w[1][0] <= w[0][0] + 1e-6, "gain not monotonic down");
        }
        // Reaches and rests at the target by the end of the window.
        assert!(set.gains_settled());
        assert!(set.gains[0].abs() < 1e-6);
        let last = out[(GAIN_RAMP_FRAMES + 49) * CHANNELS];
        assert!(last.abs() < 1e-6, "did not reach silence: {last}");
    }

    #[test]
    fn settle_snaps_gains_for_static_mix() {
        // Export-style: gains apply immediately, no audible slew at frame 0.
        let mut set = StemSet::single(constant(10, 1.0));
        set.set_gain(0, 0.0);
        set.settle();
        assert!(set.gains_settled());
        assert_eq!(set.frame(0), (0.0, 0.0));
    }
}
