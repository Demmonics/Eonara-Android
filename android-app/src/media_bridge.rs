use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use jni::objects::{JClass, JIntArray, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{Env, EnvUnowned, Outcome};
use lyricmotion_core::lyrics_service::{LookupOutcome, LyricsServiceError};
use lyricmotion_core::media_session::{MediaMetadata, MediaSessionSnapshot, TransportActions};
use lyricmotion_core::playback_clock::PlaybackState;

const MAX_PENDING_EVENTS: usize = 64;

#[derive(Debug)]
pub(crate) enum MediaBridgeEvent {
    AccessChanged(bool),
    CacheRoot(String),
    SessionSelected {
        session_id: String,
        epoch: u64,
    },
    SessionCleared {
        epoch: u64,
    },
    Snapshot {
        snapshot: MediaSessionSnapshot,
        received_at: Instant,
    },
    LyricsReady {
        session_id: String,
        epoch: u64,
        track_id: String,
        ticket: u64,
        result: Result<LookupOutcome, LyricsServiceError>,
    },
    ManualLyrics {
        session_id: Option<String>,
        track_id: String,
        epoch: u64,
        format: String,
        text: String,
    },
    Artwork {
        session_id: String,
        epoch: u64,
        revision: u64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

static EVENTS: OnceLock<Mutex<VecDeque<MediaBridgeEvent>>> = OnceLock::new();

pub(crate) fn drain_events() -> VecDeque<MediaBridgeEvent> {
    let Some(events) = EVENTS.get() else {
        return VecDeque::new();
    };
    match events.lock() {
        Ok(mut events) => std::mem::take(&mut *events),
        Err(_) => {
            log::error!("Media bridge event queue was poisoned");
            VecDeque::new()
        }
    }
}

pub(crate) fn post_lyrics_result(
    session_id: String,
    epoch: u64,
    track_id: String,
    ticket: u64,
    result: Result<LookupOutcome, LyricsServiceError>,
) {
    enqueue(MediaBridgeEvent::LyricsReady {
        session_id,
        epoch,
        track_id,
        ticket,
        result,
    });
}

fn enqueue(event: MediaBridgeEvent) {
    let events = EVENTS.get_or_init(|| Mutex::new(VecDeque::new()));
    match events.lock() {
        Ok(mut events) => {
            if events.len() == MAX_PENDING_EVENTS {
                events.pop_front();
                log::warn!("Dropping the oldest pending Android media callback");
            }
            events.push_back(event);
        }
        Err(_) => log::error!("Unable to enqueue Android media callback: queue poisoned"),
    }
}

fn read_string(
    env: &mut Env<'_>,
    value: JString<'_>,
) -> Result<Option<String>, jni::errors::Error> {
    if value.is_null() {
        return Ok(None);
    }
    value.try_to_string(env).map(Some)
}

fn with_env<T>(
    env: &mut EnvUnowned<'_>,
    operation: impl FnOnce(&mut Env<'_>) -> Result<T, jni::errors::Error>,
) -> Result<T, jni::errors::Error> {
    match env.with_env(operation).into_outcome() {
        Outcome::Ok(value) => Ok(value),
        Outcome::Err(error) => Err(error),
        Outcome::Panic(_) => {
            log::error!("Panic while processing Android JNI callback");
            Err(jni::errors::Error::JavaException)
        }
    }
}

fn nonnegative(value: jlong) -> Option<u64> {
    u64::try_from(value).ok()
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnAccessState(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    granted: jboolean,
) {
    enqueue(MediaBridgeEvent::AccessChanged(granted));
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnCacheRoot(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    path: JString<'_>,
) {
    match with_env(&mut env, |env| read_string(env, path)) {
        Ok(Some(path)) if !path.is_empty() && path.len() <= 4_096 => {
            enqueue(MediaBridgeEvent::CacheRoot(path));
        }
        Ok(_) => log::warn!("Ignoring an empty or oversized lyrics cache path"),
        Err(error) => log::warn!("Unable to read lyrics cache path: {error}"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnSessionSelected(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    session_id: JString<'_>,
    epoch: jlong,
) {
    let result = with_env(&mut env, |env| read_string(env, session_id));
    match result {
        Ok(Some(session_id)) if !session_id.is_empty() => {
            if let Some(epoch) = nonnegative(epoch) {
                enqueue(MediaBridgeEvent::SessionSelected { session_id, epoch });
            }
        }
        Ok(_) => log::warn!("Ignoring Android media session with an empty identity"),
        Err(error) => log::warn!("Unable to read Android media session identity: {error}"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnSessionCleared(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    epoch: jlong,
) {
    if let Some(epoch) = nonnegative(epoch) {
        enqueue(MediaBridgeEvent::SessionCleared { epoch });
    }
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnPlaybackSnapshot(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    session_id: JString<'_>,
    epoch: jlong,
    metadata_revision: jlong,
    revision: jlong,
    track_id: JString<'_>,
    title: JString<'_>,
    artist: JString<'_>,
    album: JString<'_>,
    position_ms: jlong,
    duration_ms: jlong,
    playback_rate: jfloat,
    playback_state: jint,
    observed_at_ms: jlong,
    actions: jint,
    artwork_revision: jlong,
) {
    let received_at = Instant::now();
    let result = with_env(&mut env, |env| {
        let session_id = read_string(env, session_id)?
            .filter(|session_id| !session_id.is_empty())
            .ok_or_else(|| jni::errors::Error::NullPtr("media session id"))?;
        let track_id = read_string(env, track_id)?;
        let title = read_string(env, title)?;
        let artist = read_string(env, artist)?;
        let album = read_string(env, album)?;
        Ok((session_id, track_id, title, artist, album))
    });

    match result {
        Ok((session_id, track_id, title, artist, album)) => {
            let Some(session_epoch) = nonnegative(epoch) else {
                log::warn!("Ignoring Android media callback with an invalid session epoch");
                return;
            };
            let Some(metadata_revision) = nonnegative(metadata_revision) else {
                log::warn!("Ignoring Android media callback with an invalid metadata revision");
                return;
            };
            let Some(revision) = nonnegative(revision) else {
                log::warn!("Ignoring Android media callback with an invalid revision");
                return;
            };
            let Some(observed_at_ms) = nonnegative(observed_at_ms) else {
                log::warn!("Ignoring Android media callback with an invalid clock anchor");
                return;
            };
            let Some(artwork_revision) = nonnegative(artwork_revision) else {
                log::warn!("Ignoring Android media callback with an invalid artwork revision");
                return;
            };
            let playback_state = match playback_state {
                1 => PlaybackState::Playing,
                2 => PlaybackState::Buffering,
                3 => PlaybackState::Stopped,
                _ => PlaybackState::Paused,
            };
            let metadata = MediaMetadata {
                track_id,
                title,
                artist,
                album,
                duration_ms: nonnegative(duration_ms).filter(|duration| *duration > 0),
                artwork_revision,
            };
            let mut snapshot = MediaSessionSnapshot {
                session_id,
                session_epoch,
                revision,
                metadata_revision,
                metadata,
                position_ms: nonnegative(position_ms).map(|position| position as f64),
                playback_state,
                playback_rate: Some(playback_rate as f64),
                observed_at_ms,
                actions: TransportActions::from_bits(actions.max(0) as u8),
            };
            if snapshot
                .metadata
                .title
                .as_ref()
                .is_some_and(|title| title.len() > 2_048)
            {
                snapshot.metadata.title = None;
            }
            if snapshot
                .metadata
                .artist
                .as_ref()
                .is_some_and(|artist| artist.len() > 2_048)
            {
                snapshot.metadata.artist = None;
            }
            if snapshot
                .metadata
                .album
                .as_ref()
                .is_some_and(|album| album.len() > 2_048)
            {
                snapshot.metadata.album = None;
            }
            enqueue(MediaBridgeEvent::Snapshot {
                snapshot,
                received_at,
            });
        }
        Err(error) => log::warn!("Unable to read Android media callback: {error}"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnArtwork(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    session_id: JString<'_>,
    epoch: jlong,
    revision: jlong,
    width: jint,
    height: jint,
    pixels: JIntArray<'_>,
) {
    let clearing = width == 0 && height == 0;
    if !clearing && (!(1..=256).contains(&width) || !(1..=256).contains(&height)) {
        log::warn!("Ignoring Android artwork with invalid dimensions");
        return;
    }
    let pixel_count = (width.max(0) as usize) * (height.max(0) as usize);
    let result = with_env(&mut env, |env| {
        let Some(session_id) = read_string(env, session_id)?.filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        if clearing {
            return Ok(Some((session_id, Vec::new())));
        }
        let length = pixels.len(env)?;
        if length != pixel_count {
            return Ok(None);
        }
        let mut argb = vec![0; pixel_count];
        pixels.get_region(env, 0, &mut argb)?;
        let mut rgba = Vec::with_capacity(pixel_count * 4);
        for pixel in argb {
            let pixel = pixel as u32;
            rgba.extend_from_slice(&[
                ((pixel >> 16) & 0xff) as u8,
                ((pixel >> 8) & 0xff) as u8,
                (pixel & 0xff) as u8,
                ((pixel >> 24) & 0xff) as u8,
            ]);
        }
        Ok(Some((session_id, rgba)))
    });

    match result {
        Ok(Some((session_id, rgba))) => {
            let Some(epoch) = nonnegative(epoch) else {
                log::warn!("Ignoring Android artwork with an invalid session epoch");
                return;
            };
            let Some(revision) = nonnegative(revision) else {
                log::warn!("Ignoring Android artwork with an invalid revision");
                return;
            };
            enqueue(MediaBridgeEvent::Artwork {
                session_id,
                epoch,
                revision,
                width: width.max(0) as u32,
                height: height.max(0) as u32,
                rgba,
            });
        }
        Ok(None) => log::warn!("Ignoring incomplete Android artwork callback"),
        Err(error) => log::warn!("Unable to read Android artwork: {error}"),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_lyricmotion_android_LyricMotionActivity_nativeOnManualLyrics(
    mut env: EnvUnowned<'_>,
    _class: JClass<'_>,
    session_id: JString<'_>,
    track_id: JString<'_>,
    epoch: jlong,
    format: JString<'_>,
    source: JString<'_>,
) {
    let result = with_env(&mut env, |env| {
        let session_id = read_string(env, session_id)?.filter(|value| !value.is_empty());
        let Some(track_id) = read_string(env, track_id)?.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let Some(format) = read_string(env, format)?.filter(|value| value.len() <= 16) else {
            return Ok(None);
        };
        let Some(source) = read_string(env, source)?.filter(|value| value.len() <= 256_000) else {
            return Ok(None);
        };
        Ok(Some((session_id, track_id, format, source)))
    });

    match result {
        Ok(Some((session_id, track_id, format, text))) => {
            let Some(epoch) = nonnegative(epoch) else {
                log::warn!("Ignoring imported lyrics with an invalid session epoch");
                return;
            };
            enqueue(MediaBridgeEvent::ManualLyrics {
                session_id,
                track_id,
                epoch,
                format,
                text,
            });
        }
        Ok(None) => log::warn!("Ignoring invalid or incomplete imported lyrics"),
        Err(error) => log::warn!("Unable to read imported lyrics: {error}"),
    }
}
