# Architecture

## M0 decision

The pinned GPUI Mobile Android implementation builds, but its public window API privately owns the WGPU renderer and does not expose a supported way for application code to submit custom WGSL to that same device. Because the shader and single-surface-ownership requirements are mandatory, M0 uses a direct Rust `wgpu` renderer as the sole owner of the Android `ANativeWindow` surface. This fallback was approved before implementation.

The current proof is a native Android `NativeActivity`; Rust handles its lifecycle and input loop, WGPU owns the Vulkan/GLES surface, WGSL draws the animated field, and `cosmic-text` shapes text into a reusable glyph texture. There is no WebView, second renderer, or Kotlin UI. The emulator selected Vulkan; that is emulator evidence, not proof of a physical device's backend.

## Intended module boundaries

- Portable Rust `core` crate: monotonic playback clock, typed media-session snapshot/actions, standard/enhanced LRC, supported TTML, plain text, Lyricsfile 1.0, LRCLIB matching, retry/cancellation and bounded memory/disk cache are implemented and core-tested. Palette math, deterministic animation timelines, and persistence beyond lyrics cache remain future work.
- Android Rust entry point: native activity/surface lifecycle, touch input, WGPU renderer, session/metadata/artwork revision guards, separate Demo/Live state and a bounded single-thread lyrics worker.
- Kotlin/JNI bridge: `MediaSessionManager`/`MediaController` observation, notification-access rationale/settings flow, automatic/manual session selection, metadata/artwork callbacks, capability-gated transport, SAF lyrics import, and typed transfer into Rust. The notification-listener component is used only to authorize active-session access and does not inspect notification content.
- JNI cache-directory handoff targets app-private cache storage. SAF reads only a user-selected file with a 256 KB limit and no broad storage permission.
- UI and GPU: one native surface owner. GPUI remains a candidate only if a future supported integration can submit custom passes through its existing renderer; do not layer a competing surface over it.

## External research

Primary animation reference: read-only Windows `docs/VERCI_VISUAL_MIGRATION_PLAN.md`. Implement deterministic absolute-media-time sampling, stable identifiers, Unicode shaping, safe mobile resizing and researched Ship/Fisheye movement. Payments, Map Journey and Notifications have no verified standalone motion measurements.

The Windows `PlaybackClock` and fixtures are porting references, not source to copy wholesale. The existing color engine documents OKLab roles and contrast behavior. LiquidMusic is inspiration only; its source license was not established.

## Remaining architectural checks

- Exercise transport-action delivery and remaining lifecycle states on physical ARM64; Spotify's metadata/artwork/lyrics path and Home/foreground surface recreation are already verified on the OnePlus 7 Pro.
- Confirm the direct renderer on a physical ARM64 device and its supported GPU backend.
- Test process death, screen-off, surface loss/recreation and permission revocation before making lifecycle guarantees.
- Keep overlay behavior behind its separate Android security and lifecycle feasibility gate.

## Eonara rebrand & upgrade continuity

- Display name: **Eonara** (*The Art of Sound*); notification-listener service label: `Eonara media session access`; debug WGPU pipeline labels and log tag (`Eonara`); LRCLIB User-Agent: `Eonara/0.1.0 (public LRCLIB lyrics lookup)`.
- Native Splash & About: `DirectRenderer` renders a 1.15 s **EONARA — THE ART OF SOUND** splash state on the single WGPU surface before transitioning to Demo/Live playback, and exposes a top-left `EONARA · ABOUT` zone (plus an **About Eonara** button in the player picker) that opens a styled `AlertDialog` with `R.drawable.eonara_poster`.
- Compatibility invariants: `applicationId` / namespace (`com.lyricmotion.android`), `liblyricmotion_android.so` library name, `LyricMotionActivity` / `LyricMotionNotificationListener` class names, and all 7 JNI native method signatures remain unchanged for seamless upgrades.
