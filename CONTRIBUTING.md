# Contributing

Contributions are welcome!

## Guidelines

1. Keep existing platform/module boundaries intact (`src-tauri/` host & IPC, `src/lyrics/` parsing/cache, `src/motion/` Three.js typography choreography, `src/color/` OKLab palettes & background shaders; on Android: `core/` portable crate and `android-app/` native `wgpu` renderer + Kotlin JNI bridge).
2. Do not submit copyrighted lyrics, proprietary font files, album artwork, API tokens, or personal telemetry.
3. Run existing unit tests and builds before opening a pull request:
   - **Windows**: `npm.cmd test`, `npm.cmd run build`, and `pwsh -NoProfile -File scripts/build-native.ps1 -Mode check`
   - **Android**: `cargo test --workspace` and `.\scripts\verify-jni-bindings.ps1` after building `.apk`

## Licensing

By submitting a contribution, you agree that it is dual-licensed under `MIT OR Apache-2.0`.
