fn main() {
    // Rubber Band is vendored (vendor/rubberband, GPLv2+) and compiled in via
    // its official single-file build, which selects the built-in FFT and
    // resampler — no librubberband/libfftw3/libsamplerate at runtime.
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("vendor/rubberband/single/RubberBandSingle.cpp")
        .include("vendor/rubberband")
        // DSP is unusable at -O0; keep it optimized even in debug builds.
        .opt_level(2)
        .debug(false)
        .warnings(false)
        .compile("rubberband");
    println!("cargo:rerun-if-changed=vendor/rubberband");
}
