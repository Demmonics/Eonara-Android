# Android floating lyrics feasibility

**Status: not investigated on-device.** Do not assume a GPUI app window can be reused as a service-hosted transparent overlay.

The separate gate must test a transparent native surface under `TYPE_APPLICATION_OVERLAY`, `Settings.canDrawOverlays`, documented user-granted `SYSTEM_ALERT_WINDOW` flow, focus/touch flags, lifecycle/revocation, and GPU rendering. Android 12+ obscured-touch protections must remain intact; do not attempt to bypass them.

If fullscreen/unrestricted click-through is unreliable, implement only a bounded lyric strip or bounded kinetic overlay with adjustable position, type size, opacity, safe margins, an immediate stop, and no display over authentication/payment screens. Verify permission revocation and emergency stop. Do not start a persistent service until a current Android foreground-service category and policy are identified; observing another app's player does not make this app a media-playback service.

No overlay APK, window, permission prompt, touch test, or device result exists yet.
