# Android migration

This document maps the approved Windows behavior to a native Android app. M0 established the GPUI Mobile custom-shader API blocker and verified the approved direct-WGPU fallback. M1 now has a native media/lyrics screen source and dual-ABI builds; runtime media behavior remains unverified.

## Source references

- Primary visual implementation specification: Windows `docs/VERCI_VISUAL_MIGRATION_PLAN.md`.
- Measurement/provenance: Windows `docs/VERCI_MOTION_ANALYSIS.md`, contact sheets, recordings, measurement JSON/CSV and offline `research/lyricmotion/comparisons/index.html`.
- Requirements and product behavior: Windows `docs/REQUIREMENTS.md`, `docs/LYRICMOTION.md`, `docs/ARCHITECTURE.md`, and `docs/VISUAL_COMPARISON.md`.
- Color: Windows `docs/COLOR_ENGINE.md`, `docs/ZEN_THEME_RESEARCH.md`, `docs/SPICETIFY_RESEARCH.md`, and `docs/PALETTE_PROTOCOL.md`.
- Known timing/parser regressions: Windows `src/model.ts`, `src/clock.test.ts`, `src/transport.test.ts`, and `src/lyrics/parse.test.ts`.

## Preserve and change

Preserve synchronized media-time reconstruction, stable lyric/cue identities, original Unicode shaping, album-color identity, stale async-result rejection, true-vs-estimated timing labels, strict Demo/Live separation, and seven background modes.

Replace visually inferior legacy scene implementations according to the new migration plan. In particular, Ship is a vertically accumulating word wall, Fisheye uses independently modeled optical roles, Codestyle reveals only available text, Branches has a traveling spline tip, Web has an irregular partial net, and Constellation retains stable clusters. Payments, Map Journey and Notifications remain independent designs without fabricated reference measurements.

## Mobile translation rules

- Compose against safe areas and display cutouts in both orientations; do not scale desktop scene coordinates down.
- Use bounded visible lyric history and stable cached layouts; reconstruct all animation state from media time after seek, resize, lifecycle interruption or style change.
- Suspend hidden gallery previews and rendering when the app is inactive or the display is off.
- Keep immersive artwork backgrounds distinct from transparent/floating lyric modes.
- Validate on actual rendered portrait and landscape frames before calling any style finished.

## Current completion

M0's direct Rust `wgpu` renderer and its x86_64 emulator captures verify the earlier native rendering proof, not the current M1 app. M1 source now includes native MediaSession observation, typed JNI snapshots, Rust clock integration, LRCLIB lookup/cache, manual lyrics import and synchronized lyric rendering. Core tests pass and both ABI APKs build and pass package audits, but the updated APKs have not been installed: no Android device is connected. Controlled-session runtime, player compatibility, physical ARM64, process recreation and surface-loss behavior are not verified.

Do not start the thirteen styles until the M1 media-to-clock-to-lyrics-to-renderer path has been exercised with a controlled native session on a device. Palette engine, production animation styles, editor, overlay and performance remain later milestones behind their own gates.
