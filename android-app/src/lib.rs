#[cfg(target_os = "android")]
#[path = "media_bridge.rs"]
mod media_bridge;

#[cfg(target_os = "android")]
#[path = "lyrics_worker.rs"]
mod lyrics_worker;

#[cfg(target_os = "android")]
mod android {
    use std::{borrow::Cow, path::PathBuf, time::Duration, time::Instant};

    use android_activity::{
        input::{InputEvent, MotionAction},
        AndroidApp, InputStatus, MainEvent, PollEvent,
    };
    use cosmic_text::{
        Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache,
    };
    use lyricmotion_core::lyrics_service::{CacheStatus, LyricsSource, TrackMetadata};
    use ndk::native_window::NativeWindow;
    use raw_window_handle::{
        AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
    };

    const TEXT_WIDTH: u32 = 1024;
    const TEXT_HEIGHT: u32 = 768;
    const FRAME_INTERVAL: Duration = Duration::from_millis(33);

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ArtworkPixels {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RenderModel {
        title: String,
        subtitle: String,
        previous_lyric: String,
        current_lyric: String,
        next_lyric: String,
        lyrics_state: String,
        playback_label: String,
        controls_label: String,
        artwork: Option<ArtworkPixels>,
        show_artwork_placeholder: bool,
    }

    impl RenderModel {
        fn demo() -> Self {
            Self {
                title: "Passing Light".into(),
                subtitle: "LyricMotion demo · Sample track".into(),
                previous_lyric: String::new(),
                current_lyric: "Words drift, then settle".into(),
                next_lyric: "Light follows the evening".into(),
                lyrics_state: "Demo lyrics · line-synced".into(),
                playback_label: "DEMO MODE".into(),
                controls_label: "Tap to move the light · Hold to import lyrics".into(),
                artwork: None,
                show_artwork_placeholder: true,
            }
        }

        fn live(
            snapshot: &lyricmotion_core::media_session::MediaSessionSnapshot,
            previous_lyric: Option<&str>,
            current_lyric: Option<&str>,
            next_lyric: Option<&str>,
            lyrics_state: &str,
            artwork: Option<ArtworkPixels>,
        ) -> Self {
            let subtitle = [
                snapshot
                    .metadata
                    .artist
                    .as_deref()
                    .filter(|value| !value.trim().is_empty()),
                snapshot
                    .metadata
                    .album
                    .as_deref()
                    .filter(|value| !value.trim().is_empty()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let playback_label = match snapshot.playback_state {
                lyricmotion_core::playback_clock::PlaybackState::Playing => "LIVE · PLAYING",
                lyricmotion_core::playback_clock::PlaybackState::Paused => "LIVE · PAUSED",
                lyricmotion_core::playback_clock::PlaybackState::Buffering => "LIVE · BUFFERING",
                lyricmotion_core::playback_clock::PlaybackState::Stopped => "LIVE · STOPPED",
            };
            let actions = &snapshot.actions;
            let mut labels = Vec::new();
            if actions.contains(lyricmotion_core::media_session::TransportActions::PREVIOUS) {
                labels.push("Previous");
            }
            if snapshot.playback_state == lyricmotion_core::playback_clock::PlaybackState::Playing {
                if actions.contains(lyricmotion_core::media_session::TransportActions::PAUSE) {
                    labels.push("Pause");
                }
            } else if actions.contains(lyricmotion_core::media_session::TransportActions::PLAY) {
                labels.push("Play");
            }
            if actions.contains(lyricmotion_core::media_session::TransportActions::NEXT) {
                labels.push("Next");
            }

            Self {
                title: snapshot.metadata.display_title().to_owned(),
                subtitle: if subtitle.is_empty() {
                    snapshot.metadata.display_artist().to_owned()
                } else {
                    subtitle
                },
                previous_lyric: previous_lyric.unwrap_or_default().to_owned(),
                current_lyric: current_lyric.unwrap_or_default().to_owned(),
                next_lyric: next_lyric.unwrap_or_default().to_owned(),
                lyrics_state: lyrics_state.to_owned(),
                playback_label: playback_label.into(),
                controls_label: format!(
                    "{} · Top right: choose player · Hold to import lyrics",
                    if labels.is_empty() {
                        "No transport actions available".into()
                    } else {
                        labels.join("       ")
                    }
                ),
                artwork,
                show_artwork_placeholder: true,
            }
        }
    }

    struct AndroidViewState {
        access_granted: bool,
        selected_session: Option<(String, u64)>,
        snapshot: Option<lyricmotion_core::media_session::MediaSessionSnapshot>,
        artwork: Option<ArtworkPixels>,
        live_clock: lyricmotion_core::playback_clock::PlaybackClock,
        live_anchor: Option<(u64, Instant)>,
        demo_clock: lyricmotion_core::playback_clock::PlaybackClock,
        demo_started: Instant,
        demo_lyrics: lyricmotion_core::lyrics::LyricsResult,
        demo_lyrics_status: String,
        live_lyrics: Option<lyricmotion_core::lyrics::LyricsResult>,
        lyrics_status: Option<String>,
        lyrics_worker: Option<super::lyrics_worker::LyricsWorker>,
        lyrics_ticket: u64,
        lyrics_lookup_identity: Option<(String, u64, String, u64)>,
    }

    impl AndroidViewState {
        fn new() -> Self {
            use lyricmotion_core::playback_clock::{
                PlaybackClock, PlaybackSnapshot, PlaybackState,
            };

            let demo_started = Instant::now();
            let mut demo_clock = PlaybackClock::default();
            demo_clock.select_session(Some("lyricmotion-demo".into()), 1);
            let _ = demo_clock.update(&PlaybackSnapshot {
                session_id: "lyricmotion-demo".into(),
                session_epoch: 1,
                revision: 1,
                track_id: Some("demo-passing-light".into()),
                position_ms: Some(0.0),
                duration_ms: Some(12_000.0),
                playback_state: PlaybackState::Playing,
                playback_rate: Some(1.0),
                observed_at_ms: 0,
                timeline_valid: true,
            });
            let demo_lyrics = lyricmotion_core::lyrics::parse_lrc(
                "[00:00.00]Words drift, then settle\n[00:03.00]Light follows the evening\n[00:06.00]A line returns in time",
                Some(12_000),
            );
            Self {
                access_granted: false,
                selected_session: None,
                snapshot: None,
                artwork: None,
                live_clock: PlaybackClock::default(),
                live_anchor: None,
                demo_clock,
                demo_started,
                demo_lyrics,
                demo_lyrics_status: "Demo lyrics · line-synced".into(),
                live_lyrics: None,
                lyrics_status: None,
                lyrics_worker: None,
                lyrics_ticket: 0,
                lyrics_lookup_identity: None,
            }
        }

        fn handle_event(&mut self, event: super::media_bridge::MediaBridgeEvent) {
            use lyricmotion_core::playback_clock::UpdateResult;

            match event {
                super::media_bridge::MediaBridgeEvent::AccessChanged(granted) => {
                    self.access_granted = granted;
                }
                super::media_bridge::MediaBridgeEvent::SessionSelected { session_id, epoch } => {
                    if self
                        .live_clock
                        .select_session(Some(session_id.clone()), epoch)
                    {
                        self.selected_session = Some((session_id, epoch));
                        self.snapshot = None;
                        self.artwork = None;
                        self.live_lyrics = None;
                        self.lyrics_status = Some("Looking up lyrics".into());
                        self.cancel_lyrics_lookup();
                        self.lyrics_lookup_identity = None;
                        self.live_anchor = None;
                        self.schedule_lyrics_lookup();
                    }
                }
                super::media_bridge::MediaBridgeEvent::SessionCleared { epoch } => {
                    if self.live_clock.select_session(None, epoch) {
                        self.selected_session = None;
                        self.snapshot = None;
                        self.artwork = None;
                        self.live_lyrics = None;
                        self.lyrics_status = None;
                        self.cancel_lyrics_lookup();
                        self.lyrics_lookup_identity = None;
                        self.live_anchor = None;
                    }
                }
                super::media_bridge::MediaBridgeEvent::CacheRoot(path) => {
                    if self.lyrics_worker.is_none() {
                        match super::lyrics_worker::LyricsWorker::new(PathBuf::from(path)) {
                            Ok(worker) => {
                                self.lyrics_worker = Some(worker);
                                self.schedule_lyrics_lookup();
                            }
                            Err(error) => {
                                log::error!("Unable to start lyrics worker: {error}");
                                self.lyrics_status =
                                    Some("Lyrics lookup could not be started".into());
                            }
                        }
                    }
                }
                super::media_bridge::MediaBridgeEvent::Snapshot {
                    snapshot,
                    received_at,
                } => {
                    if !matches!(
                        self.selected_session.as_ref(),
                        Some((session_id, epoch))
                            if session_id == &snapshot.session_id
                                && *epoch == snapshot.session_epoch
                    ) {
                        return;
                    }
                    if self.snapshot.as_ref().is_some_and(|current| {
                        snapshot.metadata_revision < current.metadata_revision
                    }) {
                        return;
                    }
                    match self.live_clock.update(&snapshot.clock_snapshot()) {
                        UpdateResult::Accepted
                        | UpdateResult::DurationOnly
                        | UpdateResult::InvalidTimeline => {
                            let track_changed = self.snapshot.as_ref().is_some_and(|current| {
                                current.metadata.track_id != snapshot.metadata.track_id
                            });
                            if track_changed {
                                self.cancel_lyrics_lookup();
                                self.live_lyrics = None;
                                self.lyrics_status = Some("Looking up lyrics".into());
                                self.artwork = None;
                                self.lyrics_lookup_identity = None;
                            }
                            self.snapshot = Some(snapshot.clone());
                            self.schedule_lyrics_lookup();
                            self.live_anchor = Some((snapshot.observed_at_ms, received_at));
                        }
                        _ => {}
                    }
                }
                super::media_bridge::MediaBridgeEvent::Artwork {
                    session_id,
                    epoch,
                    revision,
                    width,
                    height,
                    rgba,
                } => {
                    if !matches!(
                        self.selected_session.as_ref(),
                        Some((active_id, active_epoch))
                            if active_id == &session_id && *active_epoch == epoch
                    ) || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|snapshot| snapshot.metadata.artwork_revision != revision)
                    {
                        return;
                    }
                    self.artwork = if width > 0 && height > 0 {
                        Some(ArtworkPixels {
                            width,
                            height,
                            rgba,
                        })
                    } else {
                        None
                    };
                }
                super::media_bridge::MediaBridgeEvent::LyricsReady {
                    session_id,
                    epoch,
                    track_id,
                    ticket,
                    result,
                } => {
                    if ticket != self.lyrics_ticket
                        || !matches!(
                            (self.selected_session.as_ref(), self.snapshot.as_ref()),
                            (Some((active_id, active_epoch)), Some(snapshot))
                                if active_id == &session_id
                                    && *active_epoch == epoch
                                    && snapshot.metadata.track_id.as_deref()
                                        == Some(track_id.as_str())
                        )
                    {
                        return;
                    }
                    match result {
                        Ok(lyricmotion_core::lyrics_service::LookupOutcome::Found {
                            response,
                            cache,
                        }) => {
                            self.lyrics_status = Some(if cache == CacheStatus::StaleOffline {
                                format!("Cached offline · {}", source_label(response.source))
                            } else {
                                source_label(response.source).into()
                            });
                            self.live_lyrics = Some(response.lyrics);
                        }
                        Ok(lyricmotion_core::lyrics_service::LookupOutcome::NotFound) => {
                            self.live_lyrics = None;
                            self.lyrics_status = Some("Lyrics unavailable".into());
                        }
                        Ok(
                            lyricmotion_core::lyrics_service::LookupOutcome::Cancelled
                            | lyricmotion_core::lyrics_service::LookupOutcome::Stale
                            | lyricmotion_core::lyrics_service::LookupOutcome::OfflineCacheMiss,
                        ) => {}
                        Err(error) => {
                            log::warn!("Lyrics lookup failed: {error}");
                            self.live_lyrics = None;
                            self.lyrics_status = Some(error.to_string());
                        }
                    }
                }
                super::media_bridge::MediaBridgeEvent::ManualLyrics {
                    session_id,
                    epoch,
                    track_id,
                    text,
                    format,
                } => {
                    if session_id.is_none() && self.selected_session.is_none() {
                        if epoch != 0 || track_id != "lyricmotion-demo" {
                            return;
                        }
                        self.demo_lyrics = parse_imported_lyrics(&text, &format);
                        self.demo_lyrics_status = imported_quality_label(&self.demo_lyrics);
                        return;
                    }
                    if !matches!(
                        (self.selected_session.as_ref(), self.snapshot.as_ref()),
                        (Some((active_id, active_epoch)), Some(snapshot))
                            if session_id.as_deref() == Some(active_id.as_str())
                                && *active_epoch == epoch
                                && snapshot.metadata.track_id.as_deref()
                                    == Some(track_id.as_str())
                    ) {
                        return;
                    }
                    self.cancel_lyrics_lookup();
                    self.lyrics_lookup_identity = None;
                    self.live_lyrics = Some(parse_imported_lyrics(&text, &format));
                    self.lyrics_status = self.live_lyrics.as_ref().map(imported_quality_label);
                }
            }
        }

        fn schedule_lyrics_lookup(&mut self) {
            let Some(worker) = self.lyrics_worker.as_ref() else {
                return;
            };
            let Some(snapshot) = self.snapshot.as_ref() else {
                return;
            };
            let Some(title) = snapshot
                .metadata
                .title
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                if self.lyrics_lookup_identity.is_some() {
                    self.cancel_lyrics_lookup();
                    self.lyrics_lookup_identity = None;
                }
                self.lyrics_status = Some("Track title unavailable".into());
                return;
            };
            let Some(artist) = snapshot
                .metadata
                .artist
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                if self.lyrics_lookup_identity.is_some() {
                    self.cancel_lyrics_lookup();
                    self.lyrics_lookup_identity = None;
                }
                self.lyrics_status = Some("Artist information unavailable".into());
                return;
            };
            let Some((session_id, epoch)) = self.selected_session.as_ref() else {
                return;
            };
            let track_id = snapshot
                .metadata
                .track_id
                .clone()
                .filter(|track_id| !track_id.trim().is_empty())
                .unwrap_or_else(|| format!("{title}|{artist}"));
            let identity = (
                session_id.clone(),
                *epoch,
                track_id.clone(),
                snapshot.metadata_revision,
            );
            if self.lyrics_lookup_identity.as_ref() == Some(&identity) {
                return;
            }
            let job = super::lyrics_worker::LyricsJob {
                session_id: session_id.clone(),
                epoch: *epoch,
                track_id,
                metadata: TrackMetadata {
                    title: title.to_owned(),
                    artist: artist.to_owned(),
                    album: snapshot.metadata.album.clone(),
                    duration_ms: snapshot.metadata.duration_ms,
                },
            };
            match worker.submit(job) {
                Ok(ticket) => {
                    self.lyrics_ticket = ticket;
                    self.lyrics_lookup_identity = Some(identity);
                    self.lyrics_status = Some("Looking up lyrics".into());
                }
                Err(error) => {
                    log::error!("{error}");
                    self.lyrics_status = Some("Lyrics lookup could not be started".into());
                }
            }
        }

        fn cancel_lyrics_lookup(&mut self) {
            if let Some(worker) = self.lyrics_worker.as_ref() {
                match worker.cancel_current() {
                    Ok(ticket) => self.lyrics_ticket = ticket,
                    Err(error) => log::error!("{error}"),
                }
            }
        }

        fn on_lifecycle_resumed(&mut self, resumed: bool) {
            if resumed {
                self.lyrics_lookup_identity = None;
                self.schedule_lyrics_lookup();
            } else {
                self.cancel_lyrics_lookup();
                self.lyrics_lookup_identity = None;
            }
        }

        fn media_position_ms(&self) -> Option<f64> {
            match (&self.snapshot, self.live_anchor) {
                (Some(_), Some((anchor_ms, received_at))) => self
                    .live_clock
                    .at(anchor_ms.saturating_add(received_at.elapsed().as_millis() as u64)),
                _ => None,
            }
        }

        fn render_model(&self) -> RenderModel {
            let Some(snapshot) = self.snapshot.as_ref() else {
                if self.selected_session.is_some() {
                    let mut model = RenderModel::demo();
                    model.title = "Connecting to player".into();
                    model.subtitle.clear();
                    model.previous_lyric.clear();
                    model.current_lyric = "Waiting for track information".into();
                    model.next_lyric.clear();
                    model.lyrics_state = "Lyrics will appear when metadata arrives".into();
                    model.playback_label = "LIVE · CONNECTING".into();
                    model.controls_label = "Hold to import lyrics".into();
                    model.show_artwork_placeholder = false;
                    return model;
                }
                let mut model = RenderModel::demo();
                if self.access_granted {
                    model.playback_label = "DEMO MODE · NO ACTIVE PLAYER".into();
                } else {
                    model.playback_label = "DEMO MODE · MEDIA ACCESS OFF".into();
                }
                let position = self
                    .demo_clock
                    .at(self.demo_started.elapsed().as_millis() as u64);
                let context = lyric_context(&self.demo_lyrics, position);
                model.lyrics_state = self.demo_lyrics_status.clone();
                model.previous_lyric = context.0.unwrap_or_default().to_owned();
                model.current_lyric = context.1.unwrap_or_default().to_owned();
                model.next_lyric = context.2.unwrap_or_default().to_owned();
                return model;
            };

            let position = self.media_position_ms();
            let lyrics = self.live_lyrics.as_ref();
            let context = lyrics
                .map(|lyrics| lyric_context(lyrics, position))
                .unwrap_or_default();
            let quality = lyrics.map(|lyrics| match lyrics.quality {
                lyricmotion_core::lyrics::LyricsQuality::Word => "Word-synced lyrics",
                lyricmotion_core::lyrics::LyricsQuality::Mixed => "Mixed word and line timing",
                lyricmotion_core::lyrics::LyricsQuality::Line => "Line-synced lyrics",
                lyricmotion_core::lyrics::LyricsQuality::Estimated => "Estimated timing",
                lyricmotion_core::lyrics::LyricsQuality::Plain => "Unsynchronized lyrics",
                lyricmotion_core::lyrics::LyricsQuality::Unavailable => "Lyrics unavailable",
                lyricmotion_core::lyrics::LyricsQuality::Instrumental => "Instrumental",
            });
            let lyrics_state = self
                .lyrics_status
                .as_deref()
                .or(quality)
                .unwrap_or("Looking up lyrics");

            RenderModel::live(
                snapshot,
                context.0,
                context.1,
                context.2,
                lyrics_state,
                self.artwork.clone(),
            )
        }

        fn position_fraction(&self) -> Option<f32> {
            if self.snapshot.is_none() {
                if self.selected_session.is_some() {
                    return None;
                }
                let position = self
                    .demo_clock
                    .at(self.demo_started.elapsed().as_millis() as u64)?;
                let duration = self.demo_clock.duration_ms()?;
                return (duration > 0.0).then_some((position / duration) as f32);
            }
            let position = self.media_position_ms()?;
            let duration = self.live_clock.duration_ms()?;
            (duration > 0.0).then_some((position / duration) as f32)
        }
    }

    fn lyric_context(
        lyrics: &lyricmotion_core::lyrics::LyricsResult,
        position_ms: Option<f64>,
    ) -> (Option<&str>, Option<&str>, Option<&str>) {
        if lyrics.lines.is_empty() {
            return (None, None, None);
        }
        let current = if lyrics.quality == lyricmotion_core::lyrics::LyricsQuality::Plain {
            0
        } else {
            let position = position_ms.unwrap_or(0.0).max(0.0);
            lyrics
                .lines
                .iter()
                .rposition(|line| line.start_ms as f64 <= position)
                .unwrap_or(0)
        };
        (
            current
                .checked_sub(1)
                .and_then(|index| lyrics.lines.get(index))
                .map(|line| line.text.as_str()),
            lyrics.lines.get(current).map(|line| line.text.as_str()),
            lyrics
                .lines
                .get(current.saturating_add(1))
                .map(|line| line.text.as_str()),
        )
    }

    fn source_label(source: LyricsSource) -> &'static str {
        match source {
            LyricsSource::SynchronizedWords => "Word-synced lyrics",
            LyricsSource::SynchronizedLines => "Line-synced lyrics",
            LyricsSource::SynchronizedMixed => "Mixed word and line timing",
            LyricsSource::Unsynchronized => "Unsynchronized lyrics",
            LyricsSource::Instrumental => "Instrumental",
        }
    }

    fn imported_quality_label(lyrics: &lyricmotion_core::lyrics::LyricsResult) -> String {
        let quality = match lyrics.quality {
            lyricmotion_core::lyrics::LyricsQuality::Word => "Word-synced lyrics",
            lyricmotion_core::lyrics::LyricsQuality::Mixed => "Mixed word and line timing",
            lyricmotion_core::lyrics::LyricsQuality::Line => "Line-synced lyrics",
            lyricmotion_core::lyrics::LyricsQuality::Estimated => "Estimated timing",
            lyricmotion_core::lyrics::LyricsQuality::Plain => "Unsynchronized lyrics",
            lyricmotion_core::lyrics::LyricsQuality::Unavailable => "Lyrics unavailable",
            lyricmotion_core::lyrics::LyricsQuality::Instrumental => "Instrumental",
        };
        format!("Imported · {quality}")
    }

    fn parse_imported_lyrics(source: &str, format: &str) -> lyricmotion_core::lyrics::LyricsResult {
        use lyricmotion_core::lyrics::{parse_lrc, parse_plain, parse_ttml, LyricsQuality};

        match format.to_ascii_lowercase().as_str() {
            "lrc" => parse_lrc(source, None),
            "ttml" | "xml" => parse_ttml(source, None),
            "lyricsfile" | "yaml" | "yml" => {
                lyricmotion_core::lyrics::parse_lyricsfile(source, None)
            }
            _ if source.trim_start().starts_with("<") => parse_ttml(source, None),
            _ => {
                let result = parse_lrc(source, None);
                if result.quality == LyricsQuality::Unavailable {
                    parse_plain(source)
                } else {
                    result
                }
            }
        }
    }

    fn truncate_text(text: &str, max_chars: usize) -> String {
        text.chars().take(max_chars).collect()
    }

    const SHADER: &str = r#"
struct Params {
    motion: vec4<f32>,
    typography: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var text_texture: texture_2d<f32>;
@group(0) @binding(2) var text_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    let position = positions[vertex_index];
    var output: VertexOutput;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.uv = vec2<f32>((position.x + 1.0) * 0.5, (1.0 - position.y) * 0.5);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = input.uv;
    let time = params.motion.x;
    let touch = params.motion.zw;
    let flow = 0.5 + 0.5 * sin(uv.x * 3.2 + uv.y * 2.4 + time * 0.18);
    let drift = 0.5 + 0.5 * cos(uv.y * 3.8 - uv.x * 1.7 - time * 0.12);

    let moss = vec3<f32>(0.035, 0.075, 0.045);
    let sage = vec3<f32>(0.14, 0.29, 0.18);
    let clay = vec3<f32>(0.29, 0.12, 0.075);
    let ochre = vec3<f32>(0.46, 0.29, 0.105);
    var color = mix(moss, sage, smoothstep(0.08, 0.88, flow));
    color = mix(color, clay, smoothstep(0.36, 0.96, drift) * 0.72);
    color = mix(color, ochre, smoothstep(0.76, 1.0, flow + drift * 0.24) * 0.48);

    let distance_to_touch = distance(uv, touch);
    let touch_light = exp(-distance_to_touch * 5.5) * 0.18;
    color += vec3<f32>(0.52, 0.36, 0.16) * touch_light;
    let vignette = 1.0 - smoothstep(0.42, 0.92, distance(uv, vec2<f32>(0.5, 0.5))) * 0.26;
    color *= vignette;
    let grain = fract(sin(dot(floor(input.position.xy), vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
    color += vec3<f32>(grain * 0.018);

    let text_half_size = params.typography.xy * 0.5;
    let text_uv = (uv - (vec2<f32>(0.5, 0.5) - text_half_size)) / params.typography.xy;
    if all(text_uv >= vec2<f32>(0.0, 0.0)) && all(text_uv <= vec2<f32>(1.0, 1.0)) {
        let glyph = textureSample(text_texture, text_sampler, text_uv);
        color = mix(color, glyph.rgb, glyph.a);
        if params.typography.w > 0.5 && abs(text_uv.y - 0.89) < 0.004
            && text_uv.x > 0.08 && text_uv.x < 0.92 {
            let progress_x = (text_uv.x - 0.08) / 0.84;
            let progress_color = select(
                vec3<f32>(0.83, 0.80, 0.72),
                vec3<f32>(0.78, 0.57, 0.29),
                progress_x <= params.typography.z
            );
            color = mix(color, progress_color, 0.88);
        }
    }

    return vec4<f32>(color, 1.0);
}
"#;

    struct DirectRenderer {
        surface: wgpu::Surface<'static>,
        window: NativeWindow,
        _instance: wgpu::Instance,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: wgpu::SurfaceConfiguration,
        pipeline: wgpu::RenderPipeline,
        bind_group: wgpu::BindGroup,
        uniform: wgpu::Buffer,
        text_texture: wgpu::Texture,
        font_system: FontSystem,
        swash_cache: SwashCache,
        started_at: Instant,
        touches: u32,
        touch: [f32; 2],
        progress: Option<f32>,
        model: RenderModel,
    }

    impl DirectRenderer {
        async fn new(window: NativeWindow) -> Result<Self, String> {
            let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
            let instance = wgpu::Instance::new(instance_descriptor);
            let raw_window =
                RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(window.ptr().cast()));
            let raw_display = RawDisplayHandle::Android(AndroidDisplayHandle::new());

            // SAFETY: `window` is retained in `Self` for the whole `Surface` lifetime.
            let surface = unsafe {
                instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display),
                    raw_window_handle: raw_window,
                })
            }
            .map_err(|error| format!("Unable to create the Android GPU surface: {error}"))?;

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|error| format!("Unable to find a surface-compatible GPU: {error}"))?;
            let adapter_limits = adapter.limits();
            let adapter_info = adapter.get_info();
            log::info!(
                "GPU adapter: {} ({:?}); max texture dimension 2D={}",
                adapter_info.name,
                adapter_info.backend,
                adapter_limits.max_texture_dimension_2d
            );
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("LyricMotion direct WGPU device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults()
                        .using_resolution(adapter_limits),
                    memory_hints: wgpu::MemoryHints::Performance,
                    ..Default::default()
                })
                .await
                .map_err(|error| format!("Unable to create the WGPU device: {error}"))?;

            let capabilities = surface.get_capabilities(&adapter);
            let format = capabilities
                .formats
                .iter()
                .copied()
                .find(wgpu::TextureFormat::is_srgb)
                .or_else(|| capabilities.formats.first().copied())
                .ok_or_else(|| "The Android surface exposes no compatible formats".to_string())?;
            let alpha_mode = capabilities
                .alpha_modes
                .iter()
                .copied()
                .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
                .or_else(|| capabilities.alpha_modes.first().copied())
                .ok_or_else(|| "The Android surface exposes no alpha modes".to_string())?;

            let mut config = surface
                .get_default_config(
                    &adapter,
                    window.width().max(1) as u32,
                    window.height().max(1) as u32,
                )
                .ok_or_else(|| {
                    "The Android GPU surface has no default configuration".to_string()
                })?;
            config.format = format;
            config.alpha_mode = alpha_mode;
            config.present_mode = wgpu::PresentMode::Fifo;
            surface.configure(&device, &config);

            let mut font_system = FontSystem::new();
            font_system.db_mut().load_font_data(
                include_bytes!("../assets/fonts/InstrumentSerif-Regular.ttf").to_vec(),
            );
            let mut swash_cache = SwashCache::new();
            let model = RenderModel::demo();
            let text = Self::rasterize_text(&mut font_system, &mut swash_cache, &model);

            let text_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("LyricMotion shaped glyph texture"),
                size: wgpu::Extent3d {
                    width: TEXT_WIDTH,
                    height: TEXT_HEIGHT,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });

            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("LyricMotion shader parameters"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let text_view = text_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("LyricMotion glyph sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("LyricMotion shader bindings"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("LyricMotion shader resources"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&text_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("LyricMotion animated gradient WGSL"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("LyricMotion fullscreen pipeline layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("LyricMotion fullscreen gradient pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });

            let renderer = Self {
                surface,
                window,
                _instance: instance,
                device,
                queue,
                config,
                pipeline,
                bind_group,
                uniform,
                text_texture,
                font_system,
                swash_cache,
                started_at: Instant::now(),
                touches: 0,
                touch: [0.5, 0.5],
                progress: None,
                model,
            };
            renderer.upload_text(&text);
            renderer.write_uniforms();
            log::info!(
                "Direct WGPU surface ready: {}x{}, {:?}",
                renderer.config.width,
                renderer.config.height,
                renderer.config.format
            );
            Ok(renderer)
        }

        fn rasterize_text(
            font_system: &mut FontSystem,
            swash_cache: &mut SwashCache,
            model: &RenderModel,
        ) -> Vec<u8> {
            let mut pixels = vec![0; (TEXT_WIDTH * TEXT_HEIGHT * 4) as usize];
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.title,
                40.0,
                50.0,
                5,
                Color::rgba(255, 252, 246, 255),
                Family::Name("Instrument Serif"),
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.subtitle,
                22.0,
                32.0,
                60,
                Color::rgba(233, 226, 211, 235),
                Family::SansSerif,
            );
            if let Some(artwork) = model.artwork.as_ref() {
                Self::blend_artwork(&mut pixels, artwork);
            } else if model.show_artwork_placeholder {
                Self::draw_text(
                    &mut pixels,
                    font_system,
                    swash_cache,
                    "Artwork unavailable",
                    18.0,
                    26.0,
                    255,
                    Color::rgba(233, 226, 211, 175),
                    Family::SansSerif,
                );
            }
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.previous_lyric,
                21.0,
                31.0,
                368,
                Color::rgba(227, 222, 211, 130),
                Family::Name("Instrument Serif"),
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.current_lyric,
                37.0,
                48.0,
                410,
                Color::rgba(255, 251, 244, 255),
                Family::Name("Instrument Serif"),
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.next_lyric,
                21.0,
                31.0,
                472,
                Color::rgba(227, 222, 211, 145),
                Family::Name("Instrument Serif"),
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.lyrics_state,
                18.0,
                26.0,
                535,
                Color::rgba(233, 226, 211, 190),
                Family::SansSerif,
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.playback_label,
                18.0,
                26.0,
                600,
                Color::rgba(245, 239, 226, 225),
                Family::SansSerif,
            );
            Self::draw_text(
                &mut pixels,
                font_system,
                swash_cache,
                &model.controls_label,
                20.0,
                28.0,
                735,
                Color::rgba(255, 247, 232, 220),
                Family::SansSerif,
            );
            pixels
        }

        fn draw_text(
            pixels: &mut [u8],
            font_system: &mut FontSystem,
            swash_cache: &mut SwashCache,
            text: &str,
            font_size: f32,
            line_height: f32,
            y_offset: i32,
            color: Color,
            family: Family<'_>,
        ) {
            let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
            buffer.set_size(Some(TEXT_WIDTH as f32), Some(64.0));
            buffer.set_text(
                &truncate_text(text, 120),
                &Attrs::new().family(family),
                Shaping::Advanced,
                Some(Align::Center),
            );
            buffer.draw(
                font_system,
                swash_cache,
                color,
                |x, y, width, height, color| {
                    Self::blend_glyph(pixels, x, y + y_offset, width, height, color);
                },
            );
        }

        fn blend_artwork(pixels: &mut [u8], artwork: &ArtworkPixels) {
            let size = 192u32.min(artwork.width).min(artwork.height);
            if size == 0 || artwork.rgba.len() != (artwork.width * artwork.height * 4) as usize {
                return;
            }
            let origin_x = (TEXT_WIDTH - size) / 2;
            let origin_y = 126;
            for y in 0..size {
                for x in 0..size {
                    let source_x = x * artwork.width / size;
                    let source_y = y * artwork.height / size;
                    let source_offset = ((source_y * artwork.width + source_x) * 4) as usize;
                    let target_offset = (((origin_y + y) * TEXT_WIDTH + origin_x + x) * 4) as usize;
                    let alpha = artwork.rgba[source_offset + 3] as f32 / 255.0;
                    for channel in 0..3 {
                        let source = artwork.rgba[source_offset + channel] as f32;
                        let target = pixels[target_offset + channel] as f32;
                        pixels[target_offset + channel] =
                            (source * alpha + target * (1.0 - alpha)).round() as u8;
                    }
                    pixels[target_offset + 3] = 255;
                }
            }
        }

        fn blend_glyph(pixels: &mut [u8], x: i32, y: i32, width: u32, height: u32, color: Color) {
            let [red, green, blue, alpha] = color.as_rgba();
            for row in 0..height {
                let pixel_y = y.saturating_add(row as i32);
                if !(0..TEXT_HEIGHT as i32).contains(&pixel_y) {
                    continue;
                }
                for column in 0..width {
                    let pixel_x = x.saturating_add(column as i32);
                    if !(0..TEXT_WIDTH as i32).contains(&pixel_x) {
                        continue;
                    }
                    let offset = ((pixel_y as usize * TEXT_WIDTH as usize) + pixel_x as usize) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[red, green, blue, alpha]);
                }
            }
        }

        fn upload_text(&self, pixels: &[u8]) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.text_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TEXT_WIDTH * 4),
                    rows_per_image: Some(TEXT_HEIGHT),
                },
                wgpu::Extent3d {
                    width: TEXT_WIDTH,
                    height: TEXT_HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
        }

        fn write_uniforms(&self) {
            let width = self.config.width.max(1) as f32;
            let height = self.config.height.max(1) as f32;
            let scale = (width * 0.94 / TEXT_WIDTH as f32).min(height * 0.72 / TEXT_HEIGHT as f32);
            let parameters = [
                self.started_at.elapsed().as_secs_f32(),
                self.touches as f32,
                self.touch[0],
                self.touch[1],
                TEXT_WIDTH as f32 * scale / width,
                TEXT_HEIGHT as f32 * scale / height,
                self.progress.unwrap_or(0.0),
                f32::from(self.progress.is_some()),
            ];
            let mut bytes = [0; 32];
            for (chunk, value) in bytes.chunks_exact_mut(4).zip(parameters) {
                chunk.copy_from_slice(&value.to_ne_bytes());
            }
            self.queue.write_buffer(&self.uniform, 0, &bytes);
        }

        fn handle_touch(&mut self, x: f32, y: f32) {
            self.touches = self.touches.saturating_add(1);
            self.touch = [
                (x / self.config.width.max(1) as f32).clamp(0.0, 1.0),
                (y / self.config.height.max(1) as f32).clamp(0.0, 1.0),
            ];
            self.write_uniforms();
        }

        fn set_model(&mut self, model: RenderModel) {
            if self.model == model {
                return;
            }
            let pixels = Self::rasterize_text(&mut self.font_system, &mut self.swash_cache, &model);
            self.model = model;
            self.upload_text(&pixels);
        }

        fn set_progress(&mut self, progress: Option<f32>) {
            self.progress = progress.map(|value| value.clamp(0.0, 1.0));
        }

        fn resize(&mut self) {
            let width = self.window.width().max(1) as u32;
            let height = self.window.height().max(1) as u32;
            if (width, height) == (self.config.width, self.config.height) {
                return;
            }
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.write_uniforms();
        }

        fn render(&mut self) {
            self.write_uniforms();
            let frame = match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                wgpu::CurrentSurfaceTexture::Timeout
                | wgpu::CurrentSurfaceTexture::Occluded
                | wgpu::CurrentSurfaceTexture::Validation => return,
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.resize();
                    return;
                }
            };
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("LyricMotion gradient frame"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("LyricMotion gradient pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            self.queue.submit([encoder.finish()]);
            frame.present();
        }
    }

    #[no_mangle]
    fn android_main(app: AndroidApp) {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("LyricMotion"),
        );

        let mut renderer = None;
        let mut view_state = AndroidViewState::new();
        let mut resumed = false;
        let mut focused = false;
        let mut running = true;

        while running {
            let active = resumed && focused;
            let timeout = active.then_some(FRAME_INTERVAL);
            let mut pending_touch = None;
            let mut resize_requested = false;
            let mut redraw_requested = false;

            app.poll_events(timeout, |event| {
                let PollEvent::Main(event) = event else {
                    return;
                };
                match event {
                    MainEvent::InitWindow { .. } => {
                        if let Some(window) = app.native_window() {
                            match pollster::block_on(DirectRenderer::new(window)) {
                                Ok(new_renderer) => renderer = Some(new_renderer),
                                Err(error) => {
                                    log::error!("Direct WGPU initialization failed: {error}");
                                    renderer = None;
                                }
                            }
                        }
                    }
                    MainEvent::TerminateWindow { .. } => renderer = None,
                    MainEvent::WindowResized { .. }
                    | MainEvent::ConfigChanged { .. }
                    | MainEvent::InsetsChanged { .. } => resize_requested = true,
                    MainEvent::RedrawNeeded { .. } => redraw_requested = true,
                    MainEvent::InputAvailable => match app.input_events_iter() {
                        Ok(mut events) => {
                            while events.next(|input| match input {
                                InputEvent::MotionEvent(event) => {
                                    match event.action() {
                                        MotionAction::Down | MotionAction::PointerDown => {
                                            let index = event.pointer_index();
                                            if index < event.pointer_count() {
                                                let pointer = event.pointer_at_index(index);
                                                pending_touch = Some((pointer.x(), pointer.y()));
                                            }
                                        }
                                        _ => {}
                                    }
                                    InputStatus::Handled
                                }
                                _ => InputStatus::Unhandled,
                            }) {}
                        }
                        Err(error) => {
                            log::warn!("Unable to read Android input events: {error:?}");
                        }
                    },
                    MainEvent::GainedFocus => focused = true,
                    MainEvent::LostFocus => focused = false,
                    MainEvent::Resume { .. } => {
                        resumed = true;
                        view_state.on_lifecycle_resumed(true);
                    }
                    MainEvent::Pause | MainEvent::Stop => {
                        resumed = false;
                        view_state.on_lifecycle_resumed(false);
                    }
                    MainEvent::Destroy => running = false,
                    _ => {}
                }
            });

            if !running {
                break;
            }
            for event in super::media_bridge::drain_events() {
                view_state.handle_event(event);
            }
            if resize_requested {
                if let Some(renderer) = renderer.as_mut() {
                    renderer.resize();
                }
            }
            if let Some((x, y)) = pending_touch {
                if let Some(renderer) = renderer.as_mut() {
                    renderer.handle_touch(x, y);
                }
            }
            if (resumed && focused) || redraw_requested {
                if let Some(renderer) = renderer.as_mut() {
                    renderer.set_model(view_state.render_model());
                    renderer.set_progress(view_state.position_fraction());
                    renderer.render();
                }
            }
        }
    }
}
