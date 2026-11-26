# Third-party notices

## Current M0 application dependencies

The current native renderer uses `wgpu` 29.0.4, `cosmic-text` 0.19.0, `android-activity` 0.6, `android_logger` 0.15, `ndk` 0.9, `pollster` 0.4.0, `raw-window-handle` 0.6.2 and `log` 0.4. Exact versions are recorded in `Cargo.lock`. Check each pinned package's declared license and the complete transitive dependency tree before redistributing; this file is not a completed license audit.

Cargo metadata reports MIT OR Apache-2.0 for `wgpu`, `cosmic-text`, `android-activity`, `android_logger`, `ndk` and `log`; Apache-2.0 OR MIT for `pollster`; and MIT OR Apache-2.0 OR Zlib for `raw-window-handle`. The selected notices and the licenses for transitive packages still require review before release.

## GPUI feasibility investigation

The separate GPUI experiment used `itsbalamurali/gpui-mobile` at revision `5fd54f4554263ae06753e90472c7f2e2ff5d7495` and `gpui-pre` / `gpui-pre-wgpu` 0.3.6. These are no longer application dependencies after the approved renderer pivot; the pin remains documented as feasibility evidence. The inspected metadata offered Apache-2.0 as an option for GPUI Mobile and Apache-2.0 for the GPUI snapshots. Re-check the exact license texts if the GPUI experiment is restored.

## Build tools

The Gradle wrapper JAR/scripts and Android build tools are not packaged in the APK. Android SDK/NDK packages are subject to their own terms.

## Instrument Serif

The app bundles `InstrumentSerif-Regular.ttf` from Google Fonts (`ofl/instrumentserif`), licensed under SIL Open Font License 1.1. The accompanying license is at `android-app/assets/licenses/InstrumentSerif-OFL.txt`.

## M1 core dependency

The portable lyrics parser uses `roxmltree` 0.20.0, licensed `MIT OR Apache-2.0`. The dependency is pinned in `core/Cargo.toml` and `Cargo.lock`; its package metadata and source are at [crates.io](https://crates.io/crates/roxmltree/0.20.0). No upstream source files are copied into this repository.

The portable LRCLIB lyrics service adds `serde` 1.0.228, `serde_json` 1.0.145, `ureq` 2.12.1 and `unicode-normalization` 0.1.25, each licensed `MIT OR Apache-2.0`. Lyricsfile parsing adds `serde_yaml_ng` 0.10.0 (`MIT OR Apache-2.0`) and `yaml-rust2` 0.13.2 (`MIT OR Apache-2.0`). Their exact versions and the complete resolved transitive graph are recorded in `Cargo.lock`. The Lyricsfile 1.0 draft specification and format examples are dedicated to CC0 1.0 by their authors; this app implements the documented subset and does not bundle upstream examples. The TLS implementation includes rustls 0.23.46 (`Apache-2.0 OR ISC OR MIT`), ring 0.17.14 (`Apache-2.0 AND ISC`) and webpki-roots 0.26.11 (`CDLA-Permissive-2.0`). webpki-roots 1.0.12 does not declare a Cargo license expression; verify its package license before redistribution. Review current package metadata and include applicable license texts when preparing a release. The service contacts only LRCLIB's public `/api/get` endpoint; it does not use player or protected APIs. No lyric contents or track metadata are logged by this module.

No Verci reference media, LiquidMusic code/assets or proprietary reference fonts are included. Complete the Rust transitive-license audit before any release packaging.


## Inspiration & Brand Artwork Notice

- Eonara for Android is an independent open-source project inspired by kinetic lyric design such as Verci. No proprietary Verci media or assets are bundled.
- Official Eonara visual identity under `assets/brand/` and derived Android resources (`android/app/src/main/res/drawable/eonara_poster.jpg`, adaptive icons, and mipmaps) were supplied and authorized by the project owner for public distribution in the Eonara repositories.
