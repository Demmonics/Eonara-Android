# Test report

## 2026-11-25 — direct-WGPU M0

### Build environment

- JDK 21.0.8, Gradle 9.4.1, Android Gradle Plugin 9.1.0, Android Platform 35, Build Tools 36.0.0, NDK 27.2.12479018.
- Rust 1.99.1 `x86_64-pc-windows-gnu`, `aarch64-linux-android` and `x86_64-linux-android` targets, cargo-ndk 4.1.2. Cargo-ndk is configured at API 26 for `libnativewindow.so`.
- `CARGO_BUILD_JOBS=2`; JDK 21 reused. No system Java or Visual Studio components changed.

### Build and runtime evidence

- Direct Rust `wgpu` native renderer compiles for ARM64 and x86_64; Gradle packages separate debug APKs. The x86_64 APK installed and launched on Android 35 AVD `LyricMotionApi35X64`.
- Emulator logcat: `GPU adapter: NVIDIA GeForce RTX 4060 Laptop GPU (Vulkan)` and `Direct WGPU surface ready: 1080x2400, Rgba8UnormSrgb`. This is the emulator's host graphics path, not physical-device GPU evidence.
- Portrait and landscape rendered at 1080×2400 and 2400×1080. The shader-rendered gradient and OFL Instrument Serif text were visibly inspected in screenshots.
- Touch at (540,1200) moved the shader's light and advanced the on-screen touch count from 0 to 1.
- Home then foreground launch kept process ID 4689, returned the NativeActivity to `ResumedActivity`, and rendered a fresh frame. Rotation changed the output to landscape without a crash. Process recreation, display-off, pause under battery saver and physical surface teardown are not tested.
- Captures: `artifacts/m0-refined-portrait.png`, `m0-refined-touch.png`, `m0-refined-resume.png`, `m0-refined-landscape.png`. The earlier `m0-direct-wgpu-*` set records the initial visual pass. These are emulator screenshots, not deterministic lyrics fixtures.
- No Android physical device was connected; the earlier ARM64-on-emulator attempt was not treated as ARM64 runtime evidence.

### Packaging checks

- Both final APKs passed APK Signature Scheme v2 verification and `zipalign -c -v 4`.
- `aapt dump badging` confirmed package ID `com.lyricmotion.android`, min API 26, target API 35, `NativeActivity`, and the expected sole ABI for each artifact. `aapt dump permissions` showed no requested permissions.
- `apkanalyzer dex packages` found no `android.webkit.WebView` class in either package.
- `artifacts/lyricmotion-arm64-debug.apk`: 38,883,675 bytes; SHA-256 `31251C40587059BCD2C912CD880F4F369F44A75EA97A19AA53CD73F285D58890`.
- `artifacts/lyricmotion-x86_64-debug.apk`: 38,783,424 bytes; SHA-256 `8A0668D98058C4F7C861278E0AB8ED3B6CF6464A10F78B91F46D5DA9F16BC6D9`.

### Outcome and limits

- GPUI Mobile's pinned public Android renderer lacks an app-accessible WGPU device/queue/custom-pass hook. The user approved the single-owner direct-WGPU fallback; GPUI is not part of the current render path. Details are in `GPUI_FEASIBILITY.md`.
- The proof meets the M0 native rendering, WGSL, touch, font, portrait/landscape, activity pause/resume and APK-generation goals on an x86_64 emulator; physical ARM64 operation remains unknown.
- No media-session integration, lyric parser/provider, synchronized playback, production typography engine, full animation set, overlay, performance benchmark or release APK is implemented.
- `cargo fmt --all -- --check`, `cargo test -p lyricmotion-android` and `git diff --check` pass. The host test target currently contains zero tests; Android-only dependencies are target-gated so the crate can be tested on the Windows host.

## Historical baseline

The initial 2026-11-25 GPUI-only APK built for ARM64 before the emulator was installed. At that time there was no connected device, so touch, lifecycle, custom shader and screenshots were correctly reported as untested. Those results are retained as history in `GPUI_FEASIBILITY.md`, not as evidence for the current renderer.

## M1 — Portable playback and lyrics core

- Added `core` as a platform-independent workspace crate and made the Android app depend on it.
- `cargo test --workspace`: 18 core tests passed; the Android entry crate has no host unit tests.
- Clock coverage: rate projection/clamping, duplicate/duration-only reports, drift smoothing, seeks and track changes, pause/resume, buffering, unknown duration, delayed elapsed-realtime anchors, invalid timelines, normalized rates, stale anchors/revisions/session epochs, and separate Demo/Live instances.
- Lyrics coverage: standard/enhanced LRC, repeated timestamps, offsets, equal-start boundaries, trailing word ends, non-monotonic word fallback, TTML paragraphs, timed single-word spans, mixed timing quality, unsynchronized/sequential TTML fallback, plain text, opt-in estimation labels, empty/oversize input.
- At this portable-core checkpoint, both ARM64 and x86_64 `assembleNativeDebug` tasks passed with the TTML-enabled core dependency. This did not verify physical hardware or the later Android media-session source integration.
- Rebuilt both ABIs with `CARGO_BUILD_JOBS=1`, one Gradle worker and a 1 GiB Gradle heap after the default 3 GiB heap could not start under current memory limits.
- The previous x86_64 APK with the playback-core dependency was installed and launched on the Android 35 emulator; logcat confirmed the direct WGPU surface initialized with Vulkan. At that checkpoint, the core was not yet wired to the native activity; see the 2026-11-26 integration build evidence below.
- The 18-test result records the earlier independent core stage. Lyricsfile, provider/cache/cancellation and Android callback integration were added afterward; their later build and test status is recorded below.

## 2026-11-26 — M1 Android integration build

### Build environment

- Rust 1.99.1 `stable-x86_64-pc-windows-gnu`, `cargo-ndk` 4.1.2, Android NDK 27.2.12479018, SDK Platform 35, Build Tools 36.0.0, Gradle 9.4.1 and AGP 9.1.0.
- The previously recorded JDK 21 directory did not contain `bin\java.exe`; the build used the existing JDK 24.0.2 locally via `JAVA_HOME`. No global Java or PATH settings were changed.
- `CARGO_BUILD_JOBS=1`, Gradle `--no-daemon --max-workers=1`, and the existing 1 GiB Gradle heap cap were used.

### Build and package evidence

- `cargo test -p lyricmotion-core`: 33 passed, 0 failed; doc tests passed.
- Android-target Rust library compilation and Gradle Kotlin/package builds succeeded for `x86_64` and `arm64-v8a`.
- Both debug APKs passed APK Signature Scheme v2 verification and `zipalign -c -v 4`.
- `aapt dump badging` confirmed package `com.lyricmotion.android`, target API 35, and exactly the requested ABI in each APK.
- `aapt dump permissions` reports only `android.permission.INTERNET`. The media-session service uses Android's protected `BIND_NOTIFICATION_LISTENER_SERVICE` declaration and the user-granted Settings flow; SAF grants access only to a selected lyrics document.
- APK DEX inspection found no `android.webkit.WebView` reference.
- `artifacts\lyricmotion-arm64-m1-debug.apk`: 48,062,681 bytes; SHA-256 `0CA41FD44CB51604CDBD917768FB93441182E0CD663C9328F9CAF494F9279A23`.
- `artifacts\lyricmotion-x86_64-m1-debug.apk`: 47,938,622 bytes; SHA-256 `49E6E9A5D2CAA9285BC2695413DF95C7319A332D30ADDEEFA7CD25B0D2BDF616`.
- The prior M0 APKs were preserved unchanged under their original names.

### Runtime limits

- `adb devices -l` reported no connected devices. No emulator was launched or manipulated. These M1 APKs have not been installed or smoke-tested.
- Controlled media-session publisher testing, media-to-clock-to-lyrics-to-renderer runtime verification, real-player compatibility, process/surface lifecycle testing and physical ARM64 testing remain pending. The successful build does not establish runtime integration or player support.
- No APK was installed on a phone and no device permission was changed.
