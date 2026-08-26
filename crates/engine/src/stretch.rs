use crate::buffer::CHANNELS;
use crate::ffi;

/// Real-time R3 stretcher. `rate` is playback speed (RB time_ratio = 1/rate).
/// All buffers pre-allocated; `feed`/`pull` are allocation-free.
pub struct Stretcher {
    state: ffi::RubberBandState,
    // pre-allocated deinterleave/interleave scratch (BLOCK frames)
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
}

pub const BLOCK_FRAMES: usize = 1024;

unsafe impl Send for Stretcher {}

impl Stretcher {
    pub fn new() -> Self {
        let state = unsafe {
            ffi::rubberband_new(
                crate::buffer::SAMPLE_RATE,
                CHANNELS as u32,
                ffi::OPTION_PROCESS_REAL_TIME
                    | ffi::OPTION_ENGINE_FINER
                    | ffi::OPTION_PITCH_HIGH_CONSISTENCY,
                1.0,
                1.0,
            )
        };
        assert!(!state.is_null(), "rubberband_new returned null");
        Self {
            state,
            in_l: vec![0.0; BLOCK_FRAMES],
            in_r: vec![0.0; BLOCK_FRAMES],
            out_l: vec![0.0; BLOCK_FRAMES],
            out_r: vec![0.0; BLOCK_FRAMES],
        }
    }

    pub fn set_rate(&mut self, rate: f64) {
        let rate = rate.clamp(0.25, 2.0);
        unsafe { ffi::rubberband_set_time_ratio(self.state, 1.0 / rate) }
    }

    pub fn set_pitch_scale(&mut self, scale: f64) {
        unsafe { ffi::rubberband_set_pitch_scale(self.state, scale) }
    }

    /// Frames RB wants next (cap at BLOCK_FRAMES).
    pub fn frames_wanted(&self) -> usize {
        (unsafe { ffi::rubberband_get_samples_required(self.state) } as usize).min(BLOCK_FRAMES)
    }

    /// Feed interleaved stereo (≤ BLOCK_FRAMES frames).
    pub fn feed(&mut self, interleaved: &[f32]) {
        let frames = (interleaved.len() / CHANNELS).min(BLOCK_FRAMES);
        for (f, fr) in interleaved
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .take(frames)
            .enumerate()
        {
            self.in_l[f] = fr[0];
            self.in_r[f] = fr[1];
        }
        let ptrs = [self.in_l.as_ptr(), self.in_r.as_ptr()];
        unsafe { ffi::rubberband_process(self.state, ptrs.as_ptr(), frames as u32, 0) }
    }

    pub fn available(&self) -> usize {
        (unsafe { ffi::rubberband_available(self.state) }).max(0) as usize
    }

    /// Pull up to out.len()/2 frames, interleaved; returns frames written.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        // Never ask RB to retrieve more than it has buffered: on the pause
        // fade-out the pipeline pulls without feeding, and an over-request makes
        // librubberband's internal ring buffer log "N requested, only M
        // available" to stderr. Clamping is a no-op during normal playback,
        // where the feed loop guarantees available() >= the block.
        let want = (out.len() / CHANNELS)
            .min(BLOCK_FRAMES)
            .min(self.available());
        if want == 0 {
            return 0;
        }
        let ptrs = [self.out_l.as_mut_ptr(), self.out_r.as_mut_ptr()];
        let got =
            unsafe { ffi::rubberband_retrieve(self.state, ptrs.as_ptr(), want as u32) } as usize;
        for f in 0..got {
            out[f * CHANNELS] = self.out_l[f];
            out[f * CHANNELS + 1] = self.out_r[f];
        }
        got
    }

    pub fn reset(&mut self) {
        unsafe { ffi::rubberband_reset(self.state) }
    }
}

impl Default for Stretcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        unsafe { ffi::rubberband_delete(self.state) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, freq: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let s = (i as f32 / 48_000.0 * freq * std::f32::consts::TAU).sin() * 0.5;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// Push `input` through at `rate`, draining as we go.
    fn run(rate: f64, input: &[f32]) -> Vec<f32> {
        let mut st = Stretcher::new();
        st.set_rate(rate);
        let mut out = Vec::new();
        let mut fed = 0;
        let frames_in = input.len() / CHANNELS;
        let mut pull_buf = vec![0.0f32; BLOCK_FRAMES * CHANNELS];
        while fed < frames_in {
            let want = st.frames_wanted().max(1).min(frames_in - fed);
            st.feed(&input[fed * CHANNELS..(fed + want) * CHANNELS]);
            fed += want;
            while st.available() > 0 {
                let n = st.pull(&mut pull_buf);
                out.extend_from_slice(&pull_buf[..n * CHANNELS]);
                if n == 0 {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn half_rate_roughly_doubles_output_length() {
        let input = sine(48_000, 440.0);
        let out = run(0.5, &input);
        let out_frames = out.len() / CHANNELS;
        // realtime mode holds back some latency; generous bounds
        assert!((80_000..=110_000).contains(&out_frames), "{out_frames}");
    }

    #[test]
    fn unity_rate_passes_roughly_same_length() {
        let input = sine(48_000, 440.0);
        let out = run(1.0, &input);
        let out_frames = out.len() / CHANNELS;
        assert!((40_000..=50_000).contains(&out_frames), "{out_frames}");
    }

    #[test]
    fn output_is_not_silence() {
        let input = sine(48_000, 440.0);
        let out = run(0.75, &input);
        let rms = (out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
        assert!(rms > 0.1, "rms = {rms}");
    }
}
