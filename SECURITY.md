# Security Policy

## Reporting a Vulnerability

Please report security vulnerabilities responsibly by opening a private security advisory on GitHub (**Security -> Advisories -> Report a vulnerability**) rather than a public issue.

Include:
- Affected repository and commit/release tag
- Steps to reproduce or minimal proof of concept
- Potential impact

## Privacy & Network Scope

- **Windows desktop**: Reads Windows Global System Media Transport Controls (GSMTC) locally. Optional lyrics lookup queries `https://lrclib.net/api/get` with track title, artist, album, and approximate duration only when enabled. No telemetry, analytics, or Spotify/Apple credentials are collected.
- **Android**: Reads active MediaSession metadata locally via an explicitly user-granted `NotificationListenerService`. Optional LRCLIB lookup contacts `https://lrclib.net/api/get` only; local files use Android Storage Access Framework (SAF).
