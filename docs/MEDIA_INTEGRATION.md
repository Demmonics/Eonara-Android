# Media integration plan

## Native contract

Android-only Kotlin observes authorized music sessions through `MediaSessionManager`, `MediaController`, callbacks, `PlaybackState`, and `MediaMetadata`; Rust receives a typed snapshot and remains the source of playback projection and lyrics/UI decisions. The contract carries session identity/epoch, track ID, metadata and artwork revisions, playback state/position/duration/rate, `SystemClock.elapsedRealtime()` anchor and supported transport actions. Kotlin selects a playing session automatically, offers a manual chooser, gates commands on capability flags, and forwards callbacks through a bounded JNI event queue. Notification-listener access is requested through Android's documented settings flow with an explanation; the declared service does not inspect notification content. Do not require Spotify OAuth.

The contract needs session ID/package, title, artist, album, artwork bytes or content URI, duration, player position, playback rate/state, monotonic elapsed-realtime anchor, and capability flags. Transport is issued only when the selected controller reports the relevant action.

## Clock behavior

The portable Rust clock already handles monotonic anchoring; pause/resume; forward/back seeks; playback-rate updates; buffering/unknown duration; rapid track/session change; and delayed/stale callbacks. Kotlin re-observes sessions after resume. Rust cancels the active lyric request when backgrounded and re-queries on resume. Demo and Live playback clocks and lyric results remain separate.

## Lyrics

The Rust lyric engine implements LRCLIB `/api/get`, normalized title/artist matching and duration tolerance, bounded retry, stale/cancellation guards, memory and app-private disk cache with expiry/offline access, LRC/Enhanced LRC, supported TTML, plain text, Lyricsfile 1.0, and timing-quality provenance. Manual import uses Android Storage Access Framework and a 256 KB bound; it requests no broad storage permission and binds the imported result to the captured session/track generation. Lyricsfile YAML parsing rejects aliases, custom tags, duplicate/unknown fields and multi-document input. No protected lyrics interfaces or unrelated listening-history uploads.

## Support and privacy

The manifest declares `INTERNET` for the public LRCLIB provider. Notification-listener access is a user-granted special access required by `MediaSessionManager` to enumerate active sessions; it is not used to read notifications. SAF supplies a one-file read grant only when the user imports lyrics. Spotify, YouTube Music, Apple Music, Poweramp and VLC compatibility remains unverified until their actual Android media sessions have been tested. There is no OAuth. Only query relevant media-session metadata. No analytics, ad SDK, account, microphone, contacts, location, accessibility service or screen capture.

## Completion

The source integration builds for both Android ABIs and the portable core is tested. No device is connected, so the latest APKs have not been smoke-tested. A controlled Android media-session publisher and real-player test remain required; do not claim runtime integration or player compatibility until observed session behavior and available transport flags are verified on a device.
