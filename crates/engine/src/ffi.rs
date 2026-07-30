//! Hand-written bindings for the Rubber Band C API (rubberband-c.h).
//! Verified against vendor/rubberband/rubberband/rubberband-c.h (Rubber Band
//! 4.0.0, vendored and compiled in by build.rs).
#![allow(non_camel_case_types, dead_code)]
use std::os::raw::{c_int, c_uint};

pub enum RubberBandState_ {}
pub type RubberBandState = *mut RubberBandState_;
pub type RubberBandOptions = c_int;

// Verified against rubberband-c.h:
pub const OPTION_PROCESS_REAL_TIME: RubberBandOptions = 0x0000_0001;
pub const OPTION_ENGINE_FINER: RubberBandOptions = 0x2000_0000; // R3
pub const OPTION_PITCH_HIGH_CONSISTENCY: RubberBandOptions = 0x0400_0000;

extern "C" {
    pub fn rubberband_new(
        sample_rate: c_uint,
        channels: c_uint,
        options: RubberBandOptions,
        initial_time_ratio: f64,
        initial_pitch_scale: f64,
    ) -> RubberBandState;
    pub fn rubberband_delete(state: RubberBandState);
    pub fn rubberband_set_time_ratio(state: RubberBandState, ratio: f64);
    pub fn rubberband_set_pitch_scale(state: RubberBandState, scale: f64);
    pub fn rubberband_get_samples_required(state: RubberBandState) -> c_uint;
    pub fn rubberband_process(
        state: RubberBandState,
        input: *const *const f32,
        samples: c_uint,
        final_block: c_int,
    );
    pub fn rubberband_available(state: RubberBandState) -> c_int;
    pub fn rubberband_retrieve(
        state: RubberBandState,
        output: *const *mut f32,
        samples: c_uint,
    ) -> c_uint;
    pub fn rubberband_reset(state: RubberBandState);
    pub fn rubberband_get_start_delay(state: RubberBandState) -> c_uint;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_create_and_destroy_r3_realtime_stretcher() {
        unsafe {
            let s = rubberband_new(
                48000,
                2,
                OPTION_PROCESS_REAL_TIME | OPTION_ENGINE_FINER | OPTION_PITCH_HIGH_CONSISTENCY,
                1.0,
                1.0,
            );
            assert!(!s.is_null());
            assert!(rubberband_get_samples_required(s) > 0);
            rubberband_delete(s);
        }
    }
}
