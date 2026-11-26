# GPUI Mobile feasibility

**Status: GPUI gate closed with a native fallback.** The pinned GPUI Mobile revision builds, but its public renderer has no supported app-level custom-WGSL hook. With the user's approval, M0 therefore uses one direct Rust `wgpu` renderer on the native Android surface. That fallback now passes emulator rendering, touch, font, shader, orientation, pause/resume and APK-generation checks; physical ARM64 runtime remains unverified.

## Source and maturity (checked 2026-11-25)

- Project: [itsbalamurali/gpui-mobile](https://github.com/itsbalamurali/gpui-mobile)
- Candidate revision: `5fd54f4554263ae06753e90472c7f2e2ff5d7495` (latest commit observed; 2026-10-23).
- Crates.io currently exposes only `gpui-mobile` 0.1.0, published 2026-03-03. Its [docs.rs documentation](https://docs.rs/gpui-mobile/latest/gpui_mobile/) explicitly describes an early release with momentum scrolling only and says full platform implementations depend on unpublished upstream crates. That published documentation is not representative of the newer GitHub branch.
- The current GitHub README and source include Android `Platform`, `AndroidWindow`, touch input, lifecycle/event-loop handling, `android-activity`, `gpui_wgpu::WgpuRenderer`, `CosmicTextSystem`, Vulkan-preferred/GLES fallback, and a sample Android Gradle project. The upstream example documents API 26+ and ARM64 as supported; armv7/x86_64 are labeled untested.
- Current `gpui-mobile` Cargo manifest depends on `gpui-pre` and `gpui-pre-wgpu` exactly 0.3.6. `gpui-pre` / `gpui-pre-wgpu` are separately published Zed snapshots; current registry versions are newer, so validate the exact old pair rather than upgrading one side independently.
- `gpui-mobile` metadata offers `GPL-3.0-or-later OR AGPL-3.0-or-later OR Apache-2.0`; its repository includes all three license texts. `gpui-pre` and `gpui-pre-wgpu` 0.3.6 are published as Apache-2.0. Use the Apache option only after reviewing the pinned files and all transitive package notices. Do not assume the mobile implementation has desktop GPUI's maturity or support guarantees.
- The repository is active (134 commits observed and latest commit in October 2026), but the discrepancy between the stale crates.io documentation and current branch raises release/reproducibility risk. Pin an immutable revision and preserve build evidence.

## GPU/shader and ownership gate

The upstream Android `AndroidWindow` creates and owns the `WgpuRenderer` and wraps the `ANativeWindow` surface. The example's `shader_showcase.rs` draws animated GPUI gradients and circles/rings using GPUI `canvas`/`paint_quad`; it is not evidence that arbitrary WGSL can be submitted by an application view.

The proof of concept must:

1. Build on `aarch64-linux-android` and package an `arm64-v8a` APK.
2. Render GPUI-native text and controls, receive touch input, and keep a licensed custom font legible.
3. Animate an album-like gradient and run one original WGSL effect through the same GPU device/surface without claiming a second surface.
4. Survive pause/resume, surface destruction/recreation, and portrait/landscape changes.
5. Confirm the APK has no WebView dependency or WebView component.
6. Capture repeatable screenshots/frame sequences and inspect actual device/emulator output.

If GPUI's public/custom rendering API cannot provide a supported WGSL integration point, record the exact compile/API limitation and choose a single native Rust rendering stack with explicit surface ownership. Do not add a competing renderer or substitute a web container.

### Pinned API inspection

The Android `WindowState` in the pinned source owns a private `Option<WgpuRenderer>` and its `draw` method accepts only a `gpui::Scene`. `WgpuRenderer` keeps its WGPU resources private; its public API in `gpui-pre-wgpu` 0.3.6 does not provide the app a `wgpu::Device`, `Queue`, command encoder, or custom-pipeline hook. GPUI Mobile's public `AndroidWindow` / `AndroidPlatform` API likewise has no custom-WGSL submission point. The sample named `shader_showcase` paints GPUI gradients and shapes; it does not compile or submit app-owned WGSL.

This is a concrete public-API blocker for the required custom shader test with stock GPUI Mobile, not a claim that WGPU or Android lacks shader support. Do not create a second surface or device. The user approved the direct-WGPU alternative: one Rust `wgpu` renderer owns the `ANativeWindow` surface, `cosmic-text` shapes text, and Android/Kotlin remains the platform-integration boundary. The proof uses that single owner; GPUI is not embedded as a second interface/rendering stack.

## Toolchain/build record

Local SDK installation was approved. The verified build uses JDK 21, Gradle 9.4.1, Android Gradle Plugin 9.1.0, Android Platform 35, Build Tools 36.0.0 (the plugin's minimum), NDK 27.2.12479018, Rust 1.99.1 `x86_64-pc-windows-gnu`, `aarch64-linux-android`, and cargo-ndk 4.1.2. A MinGW compiler already on the machine supplies the Windows GNU host linker; the default MSVC host initially failed because `link.exe` is not installed. No Visual Studio component or system Java was changed.

The Gradle Rust task sets cargo-ndk's API level to 26 (`-P 26`), required for the NDK's `libnativewindow.so`. Build from the project root after setting `JAVA_HOME`, `ANDROID_HOME`, `ANDROID_NDK_HOME`, and `RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu`:

```powershell
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-gnu"
.\android\gradlew.bat -p android --no-daemon assembleNativeDebug
```

The installed tools are user-local. Build outputs are generated under `android\app\build`; Gradle cleans the Rust ABI output and prior debug APK before packaging to prevent stale libraries/ZIP data.

## Original GPUI-only prototype results (historical)

- Native Rust/Gradle build: successful for the pinned GPUI Mobile revision.
- APK: `android\app\build\outputs\apk\debug\app-debug.apk`, 73,586,113 bytes, SHA-256 `08AD90822190F336A1A84942D18608399A214276D3757B3A3F8E3B1A83069712`; debug signature verified (APK Signature Scheme v2).
- ABI / package: `arm64-v8a`, `com.lyricmotion.android`, min SDK 26, target SDK 35, launcher `NativeActivity`.
- Static no-WebView/permission check: APK manifest has no permissions and only the native activity; DEX package scan found no defined WebView classes; APK contains only the two arm64 Rust shared libraries for native code.
- Native UI: GPUI gradient/touch view compiles. Instrument Serif is bundled under OFL and registered through GPUI's text system. These have not been observed at runtime.
- Custom WGSL: not passed. Pinned public renderer API has no supported app-level device/queue/custom render hook (evidence above); only GPUI scene primitives are available.
- Orientation, touch behavior, lifecycle, font appearance, rendering, and frame sequences: not runtime-tested.
- Emulator/physical device: none. `adb devices -l` is empty and the emulator package is not installed.

## Direct-WGPU M0 result (2026-11-25)

- Runtime: Android 35 `LyricMotionApi35X64` emulator, x86_64 ABI, 1080×2400 portrait / 2400×1080 landscape. This is emulator operation, not a physical Android device.
- Renderer: one `wgpu` surface from the native activity, original WGSL animated gradient, reusable glyph texture and `cosmic-text` shaping with the bundled OFL Instrument Serif. Logcat reported `NVIDIA GeForce RTX 4060 Laptop GPU (Vulkan)` through emulator host graphics; this does not establish a phone GPU/backend.
- Input and lifecycle: emulator tap changed the light position and touch count. The app returned from Home to the same process/activity, remained resumed, and rendered again. Rotation produced a landscape frame without crashing and the surface adapted to 2400×1080. Process death, display-off and physical surface teardown/recreation are not covered.
- Screenshots: `artifacts/m0-refined-portrait.png`, `m0-refined-touch.png`, `m0-refined-resume.png`, and `m0-refined-landscape.png`. Earlier `m0-direct-wgpu-*` captures document the first visual iteration; no reference media is packaged.
- M0 build outputs: ARM64 debug APK `artifacts/lyricmotion-arm64-debug.apk` (38,883,675 bytes; SHA-256 `31251C40587059BCD2C912CD880F4F369F44A75EA97A19AA53CD73F285D58890`) and x86_64 emulator APK `artifacts/lyricmotion-x86_64-debug.apk` (38,783,424 bytes; SHA-256 `8A0668D98058C4F7C861278E0AB8ED3B6CF6464A10F78B91F46D5DA9F16BC6D9`). Both passed v2 signature, alignment, ABI, permission and no-WebView checks. The later M1 physical-device and APK results are in `docs/TEST_REPORT.md`.
- M0 outcome: the mandatory native rendering path is demonstrated with the approved single-owner fallback. GPUI Mobile itself did not pass the custom-WGSL requirement. No media integration, production typography engine, full UI, physical-device check, overlay or performance claim is implied.
