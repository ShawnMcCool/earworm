/// RBJ cookbook low-pass biquad, per-channel state, Direct Form 1.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn lowpass(sample_rate: f32, fc: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * fc / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cosw0 = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cosw0) / 2.0) / a0,
            b1: (1.0 - cosw0) / a0,
            b2: ((1.0 - cosw0) / 2.0) / a0,
            a1: (-2.0 * cosw0) / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn highpass(sample_rate: f32, fc: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * fc / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cosw0 = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cosw0) / 2.0) / a0,
            b1: (-(1.0 + cosw0)) / a0,
            b2: ((1.0 + cosw0) / 2.0) / a0,
            a1: (-2.0 * cosw0) / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Stereo bass-focus filter: a low-pass at 400 Hz per channel, applied
/// in-place — isolates the low end so a bassline reads clearly.
pub struct Focus {
    ch: [Vec<Biquad>; 2],
}

impl Default for Focus {
    fn default() -> Self {
        Self::new()
    }
}

impl Focus {
    pub fn new() -> Self {
        let build = || -> Vec<Biquad> {
            let sr = 48_000.0;
            let q = std::f32::consts::FRAC_1_SQRT_2;
            vec![Biquad::lowpass(sr, 400.0, q)]
        };
        Self {
            ch: [build(), build()],
        }
    }

    pub fn process_interleaved(&mut self, buf: &mut [f32]) {
        for fr in buf.as_chunks_mut::<2>().0 {
            for b in self.ch[0].iter_mut() {
                fr[0] = b.process(fr[0]);
            }
            for b in self.ch[1].iter_mut() {
                fr[1] = b.process(fr[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_through(freq: f32) -> f64 {
        let mut f = Biquad::lowpass(48_000.0, 400.0, std::f32::consts::FRAC_1_SQRT_2);
        let n = 48_000;
        let mut acc = 0.0f64;
        for i in 0..n {
            let x = (i as f32 / 48_000.0 * freq * std::f32::consts::TAU).sin();
            let y = f.process(x);
            if i > 4_800 {
                acc += (y as f64).powi(2); // skip transient
            }
        }
        (acc / (n - 4_800) as f64).sqrt()
    }

    fn rms_after(b: &mut Biquad, hz: f32) -> f32 {
        let sr = 48_000.0;
        let mut acc = 0.0f64;
        let n = 4800;
        for i in 0..n {
            let x = (std::f32::consts::TAU * hz * i as f32 / sr).sin();
            let y = b.process(x);
            if i > n / 2 {
                acc += (y * y) as f64;
            }
        }
        ((acc / (n as f64 / 2.0)).sqrt()) as f32
    }

    #[test]
    fn passes_bass_attenuates_treble() {
        let low = rms_through(100.0); // bass region
        let high = rms_through(2_000.0); // guitar/vocal region
        assert!(low > 0.6, "low rms = {low}"); // ~unity (sine rms ≈ 0.707)
        assert!(high < 0.1, "high rms = {high}"); // ≥ -17 dB
    }

    #[test]
    fn highpass_attenuates_lows_passes_highs() {
        let mut hp = Biquad::highpass(48_000.0, 1000.0, std::f32::consts::FRAC_1_SQRT_2);
        let low = rms_after(&mut hp, 100.0);
        let mut hp2 = Biquad::highpass(48_000.0, 1000.0, std::f32::consts::FRAC_1_SQRT_2);
        let high = rms_after(&mut hp2, 8000.0);
        assert!(low < 0.15, "100 Hz should be cut: {low}");
        assert!(high > 0.5, "8000 Hz should pass: {high}");
    }
}
