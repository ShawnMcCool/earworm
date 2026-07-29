use crate::buffer::{StemSet, CHANNELS, SAMPLE_RATE};
use crate::filter::Focus;
use crate::layers::{mix_layers, Layer};
use crate::looper::Looper;
use crate::metronome::{Cadence, Kit};
use crate::stretch::{Stretcher, BLOCK_FRAMES};
use std::sync::Arc;

pub const GAIN_RAMP_FRAMES: usize = 240; // 5 ms

/// Count-in click: a short sine ping with exponential decay. The first beat of
/// the count is accented (higher pitch, louder).
const CLICK_LEN_FRAMES: usize = (0.040 * SAMPLE_RATE as f64) as usize; // 40 ms
const CLICK_FREQ_NORMAL: f64 = 1000.0;
const CLICK_FREQ_ACCENT: f64 = 1500.0;
const CLICK_DECAY: f64 = 40.0;
const CLICK_AMP: f32 = 0.6;

/// One scheduled click: a beat time (song seconds) and whether it's accented
/// (a downbeat). The section-click schedule is a sorted slice of these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickMark {
    pub secs: f64,
    pub accent: bool,
}

/// The click waveform sample for a given envelope age (frames since trigger),
/// shared by the count-in pre-roll and the section-click overlay so they sound
/// identical. Silent once the envelope has decayed.
pub(crate) fn click_wave(age: usize, accent: bool, volume: f32) -> f32 {
    if age >= CLICK_LEN_FRAMES {
        return 0.0;
    }
    let t = age as f64 / SAMPLE_RATE as f64;
    let f = if accent {
        CLICK_FREQ_ACCENT
    } else {
        CLICK_FREQ_NORMAL
    };
    let env = (-CLICK_DECAY * t).exp();
    let amp = if accent { CLICK_AMP } else { CLICK_AMP * 0.7 };
    ((2.0 * std::f64::consts::PI * f * t).sin() * env) as f32 * amp * volume
}

/// A one-shot click for the section-click overlay: retrigger on each beat, mix
/// its `sample()` over the music until the envelope decays.
pub struct ClickVoice {
    age: usize,
    accent: bool,
}

impl Default for ClickVoice {
    fn default() -> Self {
        // Start decayed so a fresh voice is silent until triggered.
        Self {
            age: CLICK_LEN_FRAMES,
            accent: false,
        }
    }
}

impl ClickVoice {
    fn trigger(&mut self, accent: bool) {
        self.age = 0;
        self.accent = accent;
    }
    /// Advances the envelope by one frame, then returns the sample. Stepping
    /// first skips the always-silent age-0 sample (`sin(0) == 0`) so a freshly
    /// triggered voice is audible immediately.
    fn sample(&mut self, volume: f32) -> f32 {
        self.age = self.age.saturating_add(1);
        click_wave(self.age, self.accent, volume)
    }
}

/// Copy-only commands — safe to ship over an SPSC ring into the RT thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineCmd {
    Play,
    Pause,
    SeekSecs(f64),
    SetLoopSecs {
        start: f64,
        end: f64,
    },
    ClearLoop,
    SetRate(f64),
    /// semitones + cents, combined at the boundary into one scale factor
    SetPitchScale(f64),
    BassFocus(bool),
    /// RecallSilent: audio muted, position keeps advancing.
    Mute(bool),
    /// Per-stem mix gain (0.0..=1.5); out-of-range stems ignored.
    SetStemGain {
        idx: usize,
        gain: f32,
    },
    /// User playback volume (clamped 0.0..=1.5) — a multiplier separate
    /// from the play/pause/mute gain ramp, with its own ramp to target.
    SetVolume(f32),
    /// Configure the count-in pre-roll. `beats == 0` disables it.
    /// `beat_secs` is the 1x beat interval (60 / bpm); the pipeline divides by
    /// the current rate so the clicks track the speed fader.
    SetCountIn {
        beats: u32,
        beat_secs: f64,
        every_loop: bool,
    },
    /// Configure the free-running metronome (handled by the render core, not the
    /// pipeline). `beat_secs` is the beat interval (60 / bpm).
    SetMetronome {
        running: bool,
        beat_secs: f64,
        beats_per_bar: u32,
        strong_mask: u32,
        cadence: Cadence,
        kit: Kit,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    Position {
        secs: f64,
        rate: f64,
        playing: bool,
        /// `Some((beat, of))` while the count-in pre-roll is sounding (1-based
        /// current beat), `None` during normal playback. The playhead is held
        /// at `secs` while this is `Some`.
        count_in: Option<(u32, u32)>,
    },
    LoopWrapped,
    Finished,
    /// One metronome beat (1-based within the bar). Drives the UI bar indicator.
    MetronomeBeat {
        beat: u32,
        of: u32,
        sounded: bool,
    },
}

pub struct Pipeline {
    looper: Looper,
    stretch: Stretcher,
    focus: Option<Focus>,
    rate: f64,
    pitch_scale: f64,
    playing: bool,
    muted: bool,
    gain: f32,
    target_gain: f32,
    volume: f32,
    target_volume: f32,
    feed_buf: Vec<f32>,
    // count-in config (from SetCountIn)
    ci_beats: u32,
    ci_beat_secs: f64,
    ci_every_loop: bool,
    // count-in runtime (pre-roll in progress)
    ci_active: bool,
    ci_remaining: u32,
    ci_to_next: usize,
    ci_click_age: usize,
    ci_accent: bool,
    /// 1-based index of the beat currently sounding in the pre-roll (0 before
    /// the first), reported in Position so the UI can pulse per count.
    ci_beat_index: u32,
    /// every-loop: the current pass has been fed up to the loop end; once the
    /// stretcher drains, the next pass begins with a count-in.
    ci_pass_at_end: bool,
    // section-click overlay
    clicks: Arc<Vec<ClickMark>>,
    /// Overdub layers, mixed into the feed buffer before the stretcher.
    layers: Arc<Vec<Layer>>,
    click_cursor: usize,
    click_voice: ClickVoice,
    /// Audible song position in source frames (advances by `rate` per output
    /// frame). Decoupled from the looper's *feed* position so clicks line up
    /// with what is actually heard, not what has been fed to the stretcher.
    audible_frame: f64,
}

impl Pipeline {
    pub fn new(set: StemSet) -> Self {
        Self {
            looper: Looper::new(set),
            stretch: Stretcher::new(),
            focus: None,
            rate: 1.0,
            pitch_scale: 1.0,
            playing: false,
            muted: false,
            gain: 0.0,
            target_gain: 0.0,
            volume: 1.0,
            target_volume: 1.0,
            feed_buf: vec![0.0; BLOCK_FRAMES * CHANNELS],
            ci_beats: 0,
            ci_beat_secs: 0.5,
            ci_every_loop: false,
            ci_active: false,
            ci_remaining: 0,
            ci_to_next: 0,
            ci_click_age: 0,
            ci_accent: false,
            ci_beat_index: 0,
            ci_pass_at_end: false,
            clicks: Arc::new(Vec::new()),
            layers: Arc::new(Vec::new()),
            click_cursor: 0,
            click_voice: ClickVoice::default(),
            audible_frame: 0.0,
        }
    }

    /// Install a new section-click schedule (sorted by `secs`). Re-seeks the
    /// cursor to the current audible position; a ringing click finishes.
    pub fn set_click_schedule(&mut self, clicks: Arc<Vec<ClickMark>>) {
        self.clicks = clicks;
        self.reseek_click_cursor();
    }

    pub fn set_layers(&mut self, layers: Arc<Vec<Layer>>) {
        self.layers = layers;
    }

    /// Point the cursor at the first mark at or after the audible position.
    fn reseek_click_cursor(&mut self) {
        let pos = self.audible_frame;
        self.click_cursor = self
            .clicks
            .partition_point(|m| m.secs * SAMPLE_RATE as f64 <= pos);
    }

    /// Snap stem gains to their targets — for static-mix callers (export) that
    /// must not hear the live slew.
    pub fn settle_gains(&mut self) {
        self.looper.settle_gains();
    }

    pub fn apply(&mut self, cmd: EngineCmd) {
        match cmd {
            EngineCmd::Play => {
                self.playing = true;
                if !self.muted {
                    self.target_gain = 1.0;
                }
                if self.ci_beats > 0 {
                    self.arm_count_in();
                }
            }
            EngineCmd::Pause => {
                // Assert the pause directly. The gain ramps out for a clean
                // stop, but `playing` must not be inferred from a completed
                // ramp-down (line ~543): while muted the gain is already 0 and
                // that inference is suppressed, so an inferred pause never fires.
                self.playing = false;
                self.target_gain = 0.0;
            }
            EngineCmd::SeekSecs(secs) => {
                self.looper.seek(secs_to_frames(secs));
                self.stretch.reset();
                self.audible_frame = secs.max(0.0) * SAMPLE_RATE as f64;
                self.reseek_click_cursor();
            }
            EngineCmd::SetLoopSecs { start, end } => {
                // Only flush the stretcher if the region change actually moved the
                // playhead (it was outside the new region). A resize that keeps the
                // playhead inside is seamless — resetting would drop the buffered
                // stretch latency and audibly cut playback for ~a second.
                if self
                    .looper
                    .set_region(secs_to_frames(start), secs_to_frames(end))
                {
                    self.stretch.reset();
                    // The playhead jumped to the region start — resync the audible
                    // cursor (the reported position and the section-click walk)
                    // to it, instead of leaving it stale.
                    self.audible_frame = self.looper.pos_frames() as f64;
                    self.reseek_click_cursor();
                }
            }
            EngineCmd::ClearLoop => {
                self.looper.clear_region();
                self.ci_pass_at_end = false;
            }
            EngineCmd::SetRate(rate) => {
                self.rate = rate.clamp(0.25, 2.0);
                self.stretch.set_rate(self.rate);
            }
            EngineCmd::SetPitchScale(scale) => {
                self.pitch_scale = scale;
                self.stretch.set_pitch_scale(self.pitch_scale);
            }
            EngineCmd::BassFocus(on) => {
                self.focus = if on { Some(Focus::new()) } else { None };
            }
            EngineCmd::Mute(on) => {
                self.muted = on;
                self.target_gain = if on || !self.playing { 0.0 } else { 1.0 };
            }
            EngineCmd::SetStemGain { idx, gain } => self.looper.set_gain(idx, gain),
            EngineCmd::SetVolume(v) => self.target_volume = v.clamp(0.0, 1.5),
            EngineCmd::SetCountIn {
                beats,
                beat_secs,
                every_loop,
            } => {
                self.ci_beats = beats;
                self.ci_beat_secs = beat_secs;
                self.ci_every_loop = every_loop;
                // a config change drops any pending every-loop re-count so the
                // new mode takes effect cleanly.
                self.ci_pass_at_end = false;
            }
            EngineCmd::SetMetronome { .. } => {}
        }
    }

    /// Current audible song position in source frames (what is being heard
    /// right now, decoupled from the looper feed position). Read after `render`
    /// to anchor the playback clock to the song timeline.
    pub fn audible_song_frame(&self) -> i64 {
        self.audible_frame as i64
    }

    /// Audible song-frame advance rate in frames/sec (the song timeline rate,
    /// scaled by the speed fader). At 1× playback this is `SAMPLE_RATE`.
    pub fn song_rate_hz(&self) -> i64 {
        (SAMPLE_RATE as f64 * self.rate).round() as i64
    }

    /// Render interleaved stereo into `out`; push events into `events`.
    pub fn render(&mut self, out: &mut [f32], events: &mut Vec<EngineEvent>) {
        if self.ci_active {
            let consumed = self.render_count_in(out);
            if self.ci_active {
                // whole buffer was pre-roll
                self.push_position(events);
                return;
            }
            // pre-roll finished mid-buffer: snap the gain up so the downbeat
            // enters at full level, then render the song into the remainder.
            if !self.muted {
                self.gain = self.target_gain;
            }
            self.render_song(&mut out[consumed * CHANNELS..], events);
            self.push_position(events);
            return;
        }
        self.render_song(out, events);
        self.push_position(events);
    }

    /// Arm the count-in pre-roll: clicks sound before the next audio frame.
    fn arm_count_in(&mut self) {
        self.ci_active = true;
        self.ci_remaining = self.ci_beats;
        self.ci_to_next = 0; // first beat fires on the first frame
        self.ci_click_age = CLICK_LEN_FRAMES; // nothing sounding yet
        self.ci_beat_index = 0;
        self.stretch.reset(); // clean hand-off into the song
    }

    /// True when the pipeline drives the loop itself (feeds contiguously, capped
    /// at the loop end, and re-counts between passes) instead of letting the
    /// looper crossfade-wrap. Only in every-loop mode with a count-in and a loop.
    fn ci_loop_capping(&self) -> bool {
        self.ci_every_loop && self.ci_beats > 0 && self.looper.region().is_some()
    }

    /// One interleaved click sample for the current pre-roll frame, scaled by
    /// the volume knob. Silent once the click envelope has decayed.
    fn click_sample(&self) -> f32 {
        click_wave(self.ci_click_age, self.ci_accent, self.volume)
    }

    /// Fill `out` with count-in clicks. Returns the number of frames consumed
    /// by the pre-roll; when the pre-roll ends mid-buffer it clears `ci_active`
    /// and returns the frame index where the song should take over.
    fn render_count_in(&mut self, out: &mut [f32]) -> usize {
        let spacing = ((self.ci_beat_secs / self.rate) * SAMPLE_RATE as f64)
            .round()
            .max(1.0) as usize;
        let frames = out.len() / CHANNELS;
        for i in 0..frames {
            if self.ci_to_next == 0 {
                if self.ci_remaining > 0 {
                    self.ci_accent = self.ci_remaining == self.ci_beats; // first beat
                    self.ci_remaining -= 1;
                    self.ci_beat_index += 1;
                    self.ci_click_age = 0;
                    self.ci_to_next = spacing;
                } else {
                    // the final beat's full duration has elapsed → done
                    self.ci_active = false;
                    return i;
                }
            }
            let s = self.click_sample();
            out[i * CHANNELS] = s;
            out[i * CHANNELS + 1] = s;
            self.ci_click_age += 1;
            self.ci_to_next -= 1;
        }
        frames
    }

    /// Render the song into `out` (no count-in); push events into `events`.
    fn render_song(&mut self, out: &mut [f32], events: &mut Vec<EngineEvent>) {
        let frames_req = out.len() / CHANNELS;

        if !self.playing && self.gain == 0.0 {
            out.fill(0.0);
            return;
        }

        // Keep the stretcher fed until it can satisfy this block. In every-loop
        // mode the pipeline drives the loop: it feeds contiguously, capped at
        // the loop end (no crossfade-wrap), and once the pass is fully fed it
        // stops so the stretcher can drain before the re-count fires below.
        let capping = self.ci_loop_capping();
        while self.playing && self.stretch.available() < frames_req {
            let want = self.stretch.frames_wanted().max(1);
            if capping {
                let (_start, end) = self.looper.region().unwrap();
                let pos = self.looper.pos_frames();
                if pos >= end {
                    self.ci_pass_at_end = true;
                    break;
                }
                let cap = (end - pos).min(want);
                let n = self
                    .looper
                    .read_contiguous(&mut self.feed_buf[..cap * CHANNELS], cap);
                if n > 0 {
                    mix_layers(&self.layers, pos, &mut self.feed_buf[..n * CHANNELS]);
                    self.stretch.feed(&self.feed_buf[..n * CHANNELS]);
                }
                continue;
            }
            let src_start = self.looper.pos_frames();
            let info = self.looper.read(&mut self.feed_buf[..want * CHANNELS]);
            if info.wrapped {
                events.push(EngineEvent::LoopWrapped);
            }
            if info.frames > 0 {
                mix_layers(
                    &self.layers,
                    src_start,
                    &mut self.feed_buf[..info.frames * CHANNELS],
                );
                self.stretch.feed(&self.feed_buf[..info.frames * CHANNELS]);
            }
            if info.finished {
                events.push(EngineEvent::Finished);
                self.playing = false;
                self.target_gain = 0.0;
                break;
            }
        }

        // Pull in BLOCK_FRAMES-sized chunks until the block is filled
        // (out may be larger than one stretcher block).
        let mut filled = 0;
        while filled < frames_req {
            let got = self.stretch.pull(&mut out[filled * CHANNELS..]);
            if got == 0 {
                break;
            }
            filled += got;
        }
        out[filled * CHANNELS..].fill(0.0);

        if let Some(f) = &mut self.focus {
            f.process_interleaved(out);
        }

        // Linear ramps toward targets, applied per frame: the play/pause/mute
        // gain and the user volume each move at full-scale-per-5-ms, then the
        // frame is scaled by their product (no zipper noise on either knob).
        let step = 1.0 / GAIN_RAMP_FRAMES as f32;
        for fr in out.chunks_exact_mut(CHANNELS) {
            if self.gain < self.target_gain {
                self.gain = (self.gain + step).min(self.target_gain);
            } else if self.gain > self.target_gain {
                self.gain = (self.gain - step).max(self.target_gain);
            }
            if self.volume < self.target_volume {
                self.volume = (self.volume + step).min(self.target_volume);
            } else if self.volume > self.target_volume {
                self.volume = (self.volume - step).max(self.target_volume);
            }
            let scale = self.gain * self.volume;
            fr[0] *= scale;
            fr[1] *= scale;
        }

        // Section-click overlay: walk the audible position across the frames we
        // just produced and mix a click ping at each scheduled beat. The click
        // is added at full reference level (scaled by volume only, not the
        // play/pause gain) so it stays a steady metronome.
        if self.playing {
            let region = self.looper.region();
            let period = self.looper.loop_period_frames();
            if self.clicks.is_empty() {
                // OFF fast path: no per-frame work — bulk-advance the audible
                // cursor so it stays valid if a schedule is installed mid-play.
                self.audible_frame += filled as f64 * self.rate;
                if let (Some((_start, end)), Some(period)) = (region, period) {
                    while self.audible_frame >= end as f64 && period > 0 {
                        self.audible_frame -= period as f64;
                    }
                }
            } else {
                let vol = self.volume;
                for fr in out[..filled * CHANNELS].chunks_exact_mut(CHANNELS) {
                    let cur = self.audible_frame;
                    while self.click_cursor < self.clicks.len() {
                        let mark = self.clicks[self.click_cursor].secs * SAMPLE_RATE as f64;
                        if mark < cur {
                            self.click_cursor += 1; // stale (e.g. after a resize)
                        } else if mark < cur + self.rate {
                            self.click_voice
                                .trigger(self.clicks[self.click_cursor].accent);
                            self.click_cursor += 1;
                            break;
                        } else {
                            break;
                        }
                    }
                    let s = self.click_voice.sample(vol);
                    fr[0] += s;
                    fr[1] += s;
                    self.audible_frame += self.rate;
                    if let (Some((_start, end)), Some(period)) = (region, period) {
                        while self.audible_frame >= end as f64 && period > 0 {
                            self.audible_frame -= period as f64;
                            self.reseek_click_cursor();
                        }
                    }
                }
            }
        }

        // A completed ramp-down means pause — unless we're muted (position
        // keeps advancing while muted: RecallSilent).
        if self.target_gain == 0.0 && self.gain == 0.0 && !self.muted {
            self.playing = false;
        }

        // every-loop: the pass was fed to the loop end and the stretcher has now
        // drained, so all of this pass's audio has been output. Seek back and
        // begin the next pass with a count-in.
        if self.ci_pass_at_end && self.stretch.available() == 0 {
            self.ci_pass_at_end = false;
            if let Some((start, _end)) = self.looper.region() {
                self.looper.seek(start);
                self.audible_frame = start as f64;
                self.reseek_click_cursor();
                self.arm_count_in();
                events.push(EngineEvent::LoopWrapped);
            }
        }
    }

    fn push_position(&self, events: &mut Vec<EngineEvent>) {
        events.push(EngineEvent::Position {
            // The *audible* position — what's hitting the speakers — not the
            // looper feed cursor, which races ahead to keep the stretcher fed
            // (that made the playhead jump well ahead of the sound on play).
            secs: self.audible_frame / SAMPLE_RATE as f64,
            rate: self.rate,
            playing: self.playing,
            count_in: if self.ci_active {
                Some((self.ci_beat_index, self.ci_beats))
            } else {
                None
            },
        });
    }
}

fn secs_to_frames(secs: f64) -> usize {
    (secs.max(0.0) * SAMPLE_RATE as f64).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::buffer::SongBuffer;
    use crate::looper::XFADE_FRAMES;

    #[test]
    fn click_voice_decays_and_is_silent_until_triggered() {
        let mut v = ClickVoice::default();
        assert_eq!(v.sample(1.0), 0.0, "silent before trigger");
        v.trigger(false);
        let first = v.sample(1.0).abs();
        // advance through most of the envelope
        for _ in 0..(CLICK_LEN_FRAMES / 2) {
            v.sample(1.0);
        }
        let later = v.sample(1.0).abs();
        assert!(first > 0.0, "audible right after trigger");
        assert!(later < first, "envelope decays");
    }

    #[test]
    fn click_voice_accent_is_louder() {
        let mut normal = ClickVoice::default();
        normal.trigger(false);
        let mut accent = ClickVoice::default();
        accent.trigger(true);
        assert!(accent.sample(1.0).abs() > normal.sample(1.0).abs());
    }

    fn sine(secs: f64, hz: f32, amp: f32) -> SongBuffer {
        let frames = (secs * SAMPLE_RATE as f64) as usize;
        let mut data = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let s = (i as f32 / SAMPLE_RATE as f32 * hz * std::f32::consts::TAU).sin() * amp;
            data.push(s);
            data.push(s);
        }
        SongBuffer { data }
    }

    fn sine_buf(secs: f64) -> StemSet {
        StemSet::single(sine(secs, 220.0, 0.5))
    }

    fn render_secs(p: &mut Pipeline, secs: f64) -> (Vec<f32>, Vec<EngineEvent>) {
        let total = (secs * SAMPLE_RATE as f64) as usize;
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut block = vec![0.0f32; 256 * CHANNELS];
        let mut rendered = 0;
        while rendered < total {
            p.render(&mut block, &mut events);
            out.extend_from_slice(&block);
            rendered += 256;
        }
        (out, events)
    }

    fn rms(v: &[f32]) -> f64 {
        (v.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
    }

    #[test]
    fn paused_pipeline_renders_silence() {
        let mut p = Pipeline::new(sine_buf(2.0));
        let (out, _) = render_secs(&mut p, 0.5);
        assert_eq!(rms(&out), 0.0);
    }

    #[test]
    fn playing_renders_audio_and_position_advances() {
        let mut p = Pipeline::new(sine_buf(4.0));
        p.apply(EngineCmd::Play);
        let (out, events) = render_secs(&mut p, 1.0);
        assert!(rms(&out) > 0.2);
        let last_pos = events.iter().rev().find_map(|e| match e {
            EngineEvent::Position { secs, .. } => Some(*secs),
            _ => None,
        });
        let secs = last_pos.unwrap();
        assert!((0.5..=1.5).contains(&secs), "pos = {secs}");
    }

    #[test]
    fn loop_wrap_events_fire_with_output_period_scaled_by_rate() {
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetLoopSecs {
            start: 1.0,
            end: 2.0,
        }); // 1 s loop
        p.apply(EngineCmd::SetRate(0.5));
        p.apply(EngineCmd::Play);
        let (_, events) = render_secs(&mut p, 7.0);
        let wraps = events
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();
        // 7 s of output at half speed covers ~3.5 loop periods (minus RB latency)
        assert!((2..=4).contains(&wraps), "wraps = {wraps}");
    }

    #[test]
    fn mute_keeps_position_moving_but_output_silent() {
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetLoopSecs {
            start: 0.0,
            end: 1.0,
        });
        p.apply(EngineCmd::Play);
        p.apply(EngineCmd::Mute(true));
        let (out, events) = render_secs(&mut p, 2.5);
        // gain ramp at the start; steady state is silent
        let tail = &out[out.len() / 2..];
        assert!(rms(tail) < 1e-4, "tail rms = {}", rms(tail));
        let wraps = events
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();
        assert!(wraps >= 1, "loop must keep wrapping while muted");
    }

    #[test]
    fn pause_while_muted_stops_playback() {
        // Muting (the user's speaker toggle) shares the engine mute flag with
        // drill's mute-to-recall, so gain is already 0. Pause must still stop
        // playback — not be swallowed by the "keep advancing while muted" path.
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetLoopSecs {
            start: 0.0,
            end: 1.0,
        });
        p.apply(EngineCmd::Play);
        p.apply(EngineCmd::Mute(true));
        let _ = render_secs(&mut p, 0.5); // advance silently while muted
        p.apply(EngineCmd::Pause);
        let (_out, events) = render_secs(&mut p, 0.5);
        let last_playing = events.iter().rev().find_map(|e| match e {
            EngineEvent::Position { playing, .. } => Some(*playing),
            _ => None,
        });
        assert_eq!(
            last_playing,
            Some(false),
            "pause while muted must stop playback"
        );
    }

    #[test]
    fn bass_focus_attenuates_a_high_sine() {
        // 2 kHz sine through bass focus should drop hard
        let frames = SAMPLE_RATE as usize * 4;
        let mut data = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let s = (i as f32 / SAMPLE_RATE as f32 * 2_000.0 * std::f32::consts::TAU).sin() * 0.5;
            data.push(s);
            data.push(s);
        }
        let mut p = Pipeline::new(StemSet::single(SongBuffer { data }));
        p.apply(EngineCmd::Play);
        p.apply(EngineCmd::BassFocus(true));
        let (out, _) = render_secs(&mut p, 1.0);
        let tail = &out[out.len() / 2..];
        assert!(rms(tail) < 0.05, "rms = {}", rms(tail));
    }

    #[test]
    fn stem_gain_zero_drops_that_stem_from_the_mix() {
        // two equal-amplitude uncorrelated sines: muting one leaves the
        // other's RMS (a/√2) in the output
        let set = StemSet::new(vec![sine(4.0, 220.0, 0.4), sine(4.0, 333.0, 0.4)]);
        let mut p = Pipeline::new(set);
        p.apply(EngineCmd::Play);
        let (out, _) = render_secs(&mut p, 1.0);
        let before = rms(&out[out.len() / 2..]);
        p.apply(EngineCmd::SetStemGain { idx: 0, gain: 0.0 });
        let (out, _) = render_secs(&mut p, 1.0);
        let after = rms(&out[out.len() / 2..]);
        let one_stem = 0.4 / 2f64.sqrt();
        assert!(
            (after - one_stem).abs() / one_stem < 0.15,
            "after = {after}, expected ≈ {one_stem}"
        );
        assert!(after < before * 0.85, "before = {before}, after = {after}");
    }

    #[test]
    fn volume_half_halves_output_rms() {
        let run = |volume: Option<f32>| {
            let mut p = Pipeline::new(sine_buf(4.0));
            if let Some(v) = volume {
                p.apply(EngineCmd::SetVolume(v));
            }
            p.apply(EngineCmd::Play);
            let (out, _) = render_secs(&mut p, 1.0);
            rms(&out[out.len() / 2..]) // steady state, past both ramps
        };
        let full = run(None);
        let half = run(Some(0.5));
        assert!(full > 0.2, "full = {full}");
        assert!(
            (half - full / 2.0).abs() / (full / 2.0) < 0.05,
            "half = {half}, full = {full}"
        );
    }

    #[test]
    fn volume_clamps_to_one_point_five() {
        let run = |v: f32| {
            let mut p = Pipeline::new(sine_buf(4.0));
            p.apply(EngineCmd::SetVolume(v));
            p.apply(EngineCmd::Play);
            let (out, _) = render_secs(&mut p, 1.0);
            rms(&out[out.len() / 2..])
        };
        let max = run(1.5);
        let over = run(9.0);
        assert!(
            (over - max).abs() / max < 0.01,
            "over = {over}, max = {max}"
        );
    }

    #[test]
    fn mute_silences_regardless_of_volume() {
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetVolume(1.5));
        p.apply(EngineCmd::SetLoopSecs {
            start: 0.0,
            end: 1.0,
        });
        p.apply(EngineCmd::Play);
        p.apply(EngineCmd::Mute(true));
        let (out, _) = render_secs(&mut p, 2.5);
        let tail = &out[out.len() / 2..];
        assert!(rms(tail) < 1e-4, "tail rms = {}", rms(tail));
    }

    #[test]
    fn volume_does_not_change_loop_wrap_cadence() {
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetVolume(0.5));
        p.apply(EngineCmd::SetLoopSecs {
            start: 1.0,
            end: 2.0,
        }); // 1 s loop
        p.apply(EngineCmd::Play);
        let (_, events) = render_secs(&mut p, 3.5);
        let wraps = events
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();
        // 3.5 s of output at 1× covers ~3.5 loop periods (minus RB latency)
        assert!((2..=4).contains(&wraps), "wraps = {wraps}");
    }

    #[test]
    fn count_in_holds_looper_and_clicks_before_playback() {
        let mut p = Pipeline::new(sine_buf(4.0));
        // 2 beats at 120 bpm = 0.5 s each, no rate change → 1.0 s of pre-roll.
        p.apply(EngineCmd::SetCountIn {
            beats: 2,
            beat_secs: 0.5,
            every_loop: false,
        });
        p.apply(EngineCmd::Play);

        // During the pre-roll the looper must not advance.
        let (out, events) = render_secs(&mut p, 0.4);
        assert!(rms(&out) > 0.0, "clicks must be audible during pre-roll");
        let pos = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .unwrap();
        assert!(pos < 0.01, "looper held during pre-roll, pos = {pos}");
    }

    #[test]
    fn count_in_hands_off_to_playback_after_the_beats() {
        let mut p = Pipeline::new(sine_buf(4.0));
        p.apply(EngineCmd::SetCountIn {
            beats: 2,
            beat_secs: 0.5,
            every_loop: false,
        });
        p.apply(EngineCmd::Play);
        // 1.0 s pre-roll + 0.5 s of song.
        let (_out, events) = render_secs(&mut p, 1.5);
        let pos = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .unwrap();
        assert!(
            pos > 0.2,
            "song must advance after the pre-roll, pos = {pos}"
        );
    }

    #[test]
    fn count_in_pre_roll_scales_with_rate() {
        // At half speed the pre-roll lasts twice as long, so after 1.0 s the
        // song has not started yet (2 beats * 0.5 s / 0.5 rate = 2.0 s pre-roll).
        let mut p = Pipeline::new(sine_buf(6.0));
        p.apply(EngineCmd::SetRate(0.5));
        p.apply(EngineCmd::SetCountIn {
            beats: 2,
            beat_secs: 0.5,
            every_loop: false,
        });
        p.apply(EngineCmd::Play);
        let (_out, events) = render_secs(&mut p, 1.0);
        let pos = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .unwrap();
        assert!(
            pos < 0.01,
            "rate-scaled pre-roll still running, pos = {pos}"
        );
    }

    #[test]
    fn count_in_zero_beats_is_a_no_op() {
        let mut p = Pipeline::new(sine_buf(4.0));
        p.apply(EngineCmd::SetCountIn {
            beats: 0,
            beat_secs: 0.5,
            every_loop: false,
        });
        p.apply(EngineCmd::Play);
        let (_out, events) = render_secs(&mut p, 0.5);
        let pos = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .unwrap();
        assert!(
            pos > 0.2,
            "no count-in → song starts immediately, pos = {pos}"
        );
    }

    #[test]
    fn count_in_reports_beat_index_then_clears() {
        let mut p = Pipeline::new(sine_buf(4.0));
        p.apply(EngineCmd::SetCountIn {
            beats: 3,
            beat_secs: 0.5,
            every_loop: false,
        });
        p.apply(EngineCmd::Play);
        // 1.5 s pre-roll (3 × 0.5 s) then song.
        let (_out, events) = render_secs(&mut p, 2.5);

        // While counting in, the beat index runs 1..=3 over the pre-roll.
        let beats: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Position {
                    count_in: Some((b, of)),
                    ..
                } => {
                    assert_eq!(*of, 3, "total beats reported");
                    Some(*b)
                }
                _ => None,
            })
            .collect();
        assert_eq!(beats.iter().min(), Some(&1));
        assert_eq!(beats.iter().max(), Some(&3));

        // After the pre-roll, count_in clears so the playhead resumes.
        let last = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::Position { count_in, .. } => Some(*count_in),
                _ => None,
            })
            .unwrap();
        assert_eq!(last, None, "count_in clears once playback begins");
    }

    #[test]
    fn every_loop_inserts_count_in_between_passes() {
        // Baseline: a 1 s loop with count-in in first-loop mode wraps seamlessly
        // after the single initial pre-roll.
        let mut seamless = Pipeline::new(sine_buf(10.0));
        seamless.apply(EngineCmd::SetLoopSecs {
            start: 0.0,
            end: 1.0,
        });
        seamless.apply(EngineCmd::SetCountIn {
            beats: 2,
            beat_secs: 0.5,
            every_loop: false,
        });
        seamless.apply(EngineCmd::Play);
        let (_o, ev_seamless) = render_secs(&mut seamless, 6.0);
        let wraps_seamless = ev_seamless
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();

        // Every-loop: each pass is preceded by a 1 s count-in, so fewer passes
        // fit in the same wall-clock — the clicks are inserted between passes.
        let mut every = Pipeline::new(sine_buf(10.0));
        every.apply(EngineCmd::SetLoopSecs {
            start: 0.0,
            end: 1.0,
        });
        every.apply(EngineCmd::SetCountIn {
            beats: 2,
            beat_secs: 0.5,
            every_loop: true,
        });
        every.apply(EngineCmd::Play);
        let (_o2, ev_every) = render_secs(&mut every, 6.0);
        let wraps_every = ev_every
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();

        assert!(
            wraps_every >= 1,
            "every-loop must re-count and wrap, got {wraps_every}"
        );
        assert!(
            wraps_every < wraps_seamless,
            "count-in between passes means fewer wraps: every={wraps_every} seamless={wraps_seamless}"
        );

        // The pipeline-driven loop never plays past the loop end into the rest
        // of the song.
        let max_pos = ev_every
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .fold(0.0_f64, f64::max);
        assert!(
            max_pos <= 1.05,
            "every-loop stays within the region, max_pos = {max_pos}"
        );
    }

    #[test]
    fn position_tracks_audible_output_not_feed_cursor() {
        // At 1x from the top, the reported position must equal the audio actually
        // produced — it must not leap ahead by the stretcher's feed-ahead buffer.
        // (The bug: the playhead jumped well past the sound the moment you hit
        // play, because the position reported the looper feed cursor.)
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::Play);
        let (out, events) = render_secs(&mut p, 1.0);
        let produced = (out.len() / CHANNELS) as f64 / SAMPLE_RATE as f64;
        let max_pos = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .fold(0.0_f64, f64::max);
        assert!(
            max_pos <= produced + 0.01,
            "position {max_pos} leapt past produced audio {produced}"
        );
        assert!(max_pos > 0.5, "position did not advance: {max_pos}");
    }

    #[test]
    fn every_loop_stays_in_region_when_slowed_and_offset() {
        // The real use case: a slowed-down loop that does not start at 0. The
        // pipeline-driven loop must re-count and stay inside [2, 3] regardless.
        let mut p = Pipeline::new(sine_buf(10.0));
        p.apply(EngineCmd::SetRate(0.5));
        p.apply(EngineCmd::SetLoopSecs {
            start: 2.0,
            end: 3.0,
        });
        p.apply(EngineCmd::SetCountIn {
            beats: 1,
            beat_secs: 0.5,
            every_loop: true,
        });
        p.apply(EngineCmd::Play);
        let (_o, events) = render_secs(&mut p, 8.0);

        let wraps = events
            .iter()
            .filter(|e| **e == EngineEvent::LoopWrapped)
            .count();
        assert!(wraps >= 1, "slowed offset loop must re-count, got {wraps}");

        let positions: Vec<f64> = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Position { secs, .. } => Some(*secs),
                _ => None,
            })
            .collect();
        let max_pos = positions.iter().cloned().fold(0.0_f64, f64::max);
        let min_pos = positions.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            max_pos <= 3.05,
            "must not play past the loop end, max = {max_pos}"
        );
        assert!(
            min_pos >= 1.95,
            "must not play before the loop start, min = {min_pos}"
        );
    }

    #[test]
    fn section_click_mixes_over_audio_at_scheduled_beat() {
        use std::sync::Arc;
        // 1s of silence so the click overlay is the only output signal.
        let song = StemSet::new(vec![sine(1.0, 440.0, 0.0)]);
        let mut p = Pipeline::new(song);
        p.set_click_schedule(Arc::new(vec![ClickMark {
            secs: 0.5,
            accent: false,
        }]));
        p.apply(EngineCmd::Play);

        let mut out = vec![0.0f32; 256 * CHANNELS];
        let mut events = Vec::new();
        let mut first_loud: Option<usize> = None;
        let mut frame = 0usize;
        for _ in 0..(SAMPLE_RATE as usize / 256) {
            out.iter_mut().for_each(|s| *s = 0.0);
            p.render(&mut out, &mut events);
            for i in 0..256 {
                if first_loud.is_none() && out[i * CHANNELS].abs() > 0.05 {
                    first_loud = Some(frame + i);
                }
            }
            frame += 256;
            if frame as f64 / SAMPLE_RATE as f64 > 0.7 {
                break;
            }
        }
        let at = first_loud.expect("a click sounded");
        let expected = (0.5 * SAMPLE_RATE as f64) as usize;
        assert!(
            (at as i64 - expected as i64).abs() < (SAMPLE_RATE as i64 / 50),
            "click at frame {at}, expected ~{expected}"
        );
    }

    #[test]
    fn section_click_does_not_drift_across_loop_passes() {
        use std::sync::Arc;
        // Silent 4s song; loop [1.0, 3.0). One click at 1.5s (0.5s into loop).
        let song = StemSet::new(vec![sine(4.0, 440.0, 0.0)]);
        let mut p = Pipeline::new(song);
        p.set_click_schedule(Arc::new(vec![ClickMark {
            secs: 1.5,
            accent: false,
        }]));
        p.apply(EngineCmd::SetLoopSecs {
            start: 1.0,
            end: 3.0,
        });
        p.apply(EngineCmd::SeekSecs(1.0));
        p.apply(EngineCmd::Play);

        let block = 256usize;
        let mut out = vec![0.0f32; block * CHANNELS];
        let mut events = Vec::new();
        // Absolute output frame of each click onset. The click ping is a
        // decaying sine that crosses the threshold many times within its ~40 ms
        // envelope, so debounce: an onset is the first loud frame after a quiet
        // gap longer than the envelope.
        let mut onsets: Vec<i64> = Vec::new();
        let mut global = 0i64;
        let debounce = SAMPLE_RATE as i64 / 10; // 100 ms
        let passes = (6.0 * SAMPLE_RATE as f64) as usize / block;
        for _ in 0..passes {
            out.iter_mut().for_each(|s| *s = 0.0);
            p.render(&mut out, &mut events);
            for i in 0..block {
                let v = out[i * CHANNELS].abs();
                if v > 0.05 {
                    let at = global + i as i64;
                    if onsets.last().is_none_or(|&prev| at - prev > debounce) {
                        onsets.push(at);
                    }
                }
            }
            global += block as i64;
        }
        assert!(
            onsets.len() >= 3,
            "expected multiple click passes, got {onsets:?}"
        );
        // Consecutive onsets are spaced by the audible loop period (len - xfade),
        // not the full region length. The old (end - start) wrap inflates the
        // spacing by xfade (~10 ms) every pass — that is the drift.
        let period = (2.0 * SAMPLE_RATE as f64) as i64 - XFADE_FRAMES as i64;
        let gaps: Vec<i64> = onsets.windows(2).map(|w| w[1] - w[0]).collect();
        for &g in &gaps {
            assert!(
                (g - period).abs() < SAMPLE_RATE as i64 / 100, // <10 ms tolerance
                "click spacing {g} != audible period {period}; gaps = {gaps:?}"
            );
        }
    }

    #[test]
    fn no_clicks_when_schedule_empty() {
        let song = StemSet::new(vec![sine(1.0, 440.0, 0.0)]);
        let mut p = Pipeline::new(song);
        p.apply(EngineCmd::Play);
        let mut out = vec![0.0f32; 256 * CHANNELS];
        let mut events = Vec::new();
        for _ in 0..20 {
            out.iter_mut().for_each(|s| *s = 0.0);
            p.render(&mut out, &mut events);
            assert!(
                out.iter().all(|s| s.abs() < 1e-6),
                "silent with no schedule"
            );
        }
    }

    #[test]
    fn finished_event_after_song_end_without_loop() {
        let mut p = Pipeline::new(sine_buf(0.5));
        p.apply(EngineCmd::Play);
        let (_, events) = render_secs(&mut p, 1.5);
        assert!(events.contains(&EngineEvent::Finished));
    }
}
