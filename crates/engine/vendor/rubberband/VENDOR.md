# Vendored: Rubber Band Library 4.0.0

- Source: https://breakfastquay.com/files/releases/rubberband-4.0.0.tar.bz2
- License: GPL-2.0-or-later (see `COPYING`); commercial licensing available
  from Particular Programs Ltd.
- Pruned from the upstream tarball: everything except `rubberband/` (public
  headers), `src/` (minus `jni/` and `test/`), `single/`, `COPYING`,
  `CHANGELOG`, `README.md`.
- Built by `crates/engine/build.rs` as the official single-file unit
  (`single/RubberBandSingle.cpp`): built-in FFT + BQResampler, no FFTW or
  libsamplerate. Real-time mode only (the engine always sets
  `OPTION_PROCESS_REAL_TIME`, so the single-file build's `NO_THREADING` is
  irrelevant).

To upgrade: replace this directory's contents from the new tarball with the
same pruning, then re-verify `src/ffi.rs` against
`rubberband/rubberband-c.h`.
