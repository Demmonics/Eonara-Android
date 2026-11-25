# Changelog

## M0 — Native rendering feasibility

- Investigated and built the pinned GPUI Mobile Android candidate; documented the missing public custom-WGSL hook.
- With user approval, switched the proof to one direct Rust `wgpu` renderer owning the native Android surface; added WGSL animated gradients, `cosmic-text` shaping, Instrument Serif and touch response.
- Built ARM64 and x86_64 debug APKs. Verified the x86_64 package on an Android 35 emulator for touch, portrait/landscape and Home/resume.
- Updated architecture, feasibility, licensing and test evidence. Physical ARM64 operation, product features, overlays and performance remain unverified.

## M1 — Playback foundation

- Added the portable `lyricmotion-core` crate with monotonic clock projection, correction smoothing, seeks, pause/resume, playback-rate bounds and duration handling.
- Added standard/enhanced LRC and plain parsing with offsets, repeated stamps, explicit timing quality, and opt-in estimated cues.
- Added TTML line and supported single-word span timing; untimed and sequential documents remain explicitly unsynchronized.
- Mixed word/line timing is labeled separately so partial word timing is not overstated.
- Added stale session/sequence/anchor guards and 18 tests covering playback, LRC and TTML timing behavior.
- Linked the core crate to the Android application and confirmed ARM64/x86_64 builds.
- Added `roxmltree` 0.20.0 (MIT OR Apache-2.0) as the bounded XML parser dependency.
- Lyrics and media-session integration remain in progress.
