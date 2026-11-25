//! Portable, blocking LRCLIB lookup with bounded retries and app-owned memory/disk caching.
//!
//! Call [`LyricsService::lookup`] away from UI/render threads. The default HTTPS transport has
//! bounded connect/read/write timeouts and response size limits. Cancellation and request
//! generations suppress results, but cannot interrupt an in-progress synchronous socket call;
//! the timeout bounds how long that call can remain blocked. Supply an app-private cache root;
//! lyric text is stored there without logging or encryption by this module.
//!
//! Synchronized LRC and unsynchronized text are passed through the existing parser without
//! relabeling its [`LyricsQuality`] result. Use [`LyricsService::begin_offline_lookup`] to avoid
//! network access and return cached entries even after their normal freshness period.

use crate::lyrics::{parse_lrc, parse_lyricsfile, parse_plain, LyricsQuality, LyricsResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

const ENDPOINT: &str = "https://lrclib.net/api/get";
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_TRACK_FIELD_CHARS: usize = 512;
const MAX_MEMORY_CACHE_ENTRIES: usize = 8;
const MAX_DISK_CACHE_ENTRIES: usize = 128;
const MAX_DISK_CACHE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RETRY_ATTEMPTS: u8 = 5;
const CACHE_FORMAT_VERSION: u8 = 1;
const DEFAULT_CACHE_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Track metadata supplied by the playback integration. No player APIs are used by this module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
}

impl TrackMetadata {
    pub fn new(title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            album: None,
            duration_ms: None,
        }
    }

    fn validate(&self) -> Result<(), LyricsServiceError> {
        for value in [&self.title, &self.artist] {
            if value.trim().is_empty() || value.chars().count() > MAX_TRACK_FIELD_CHARS {
                return Err(LyricsServiceError::InvalidTrack);
            }
        }
        if self
            .album
            .as_ref()
            .is_some_and(|album| album.chars().count() > MAX_TRACK_FIELD_CHARS)
            || self
                .duration_ms
                .is_some_and(|duration| duration > 86_400_000)
        {
            return Err(LyricsServiceError::InvalidTrack);
        }
        Ok(())
    }
}

/// Normalized for matching and cache keys; display metadata remains unchanged.
pub fn normalize_title(value: &str) -> String {
    normalize_identity(strip_feature_suffix(value))
}

/// Normalized for matching and cache keys; display metadata remains unchanged.
pub fn normalize_artist(value: &str) -> String {
    normalize_identity(value)
}

fn strip_feature_suffix(value: &str) -> &str {
    let mut cut_at = None;
    let markers = ["featuring ", "feat.", "feat ", "ft.", "ft "];
    for (position, character) in value.char_indices() {
        if !matches!(character, 'f' | 'F') {
            continue;
        }
        let is_token_boundary = position == 0
            || !value[..position]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        let suffix = &value.as_bytes()[position..];
        if is_token_boundary
            && markers.iter().any(|marker| {
                suffix
                    .get(..marker.len())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(marker.as_bytes()))
            })
        {
            cut_at = Some(position);
            break;
        }
    }
    cut_at.map_or(value, |position| &value[..position])
}

fn normalize_identity(value: &str) -> String {
    let mut normalized = String::new();
    for character in value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
    {
        if character == '&' {
            if !normalized.is_empty() && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
            normalized.push_str("and ");
            continue;
        }
        for lower in character.to_lowercase() {
            if lower.is_alphanumeric() {
                normalized.push(lower);
            } else if !normalized.is_empty() && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
        }
    }
    normalized.trim().to_owned()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LyricsSource {
    SynchronizedWords,
    SynchronizedLines,
    SynchronizedMixed,
    Unsynchronized,
    Instrumental,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheStatus {
    Memory,
    Disk,
    StaleOffline,
    Network,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricsResponse {
    pub lyrics: LyricsResult,
    /// Describes the provider's source independently of the parser's quality classification.
    pub source: LyricsSource,
    pub track_title: String,
    pub artist: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LookupOutcome {
    Found {
        response: LyricsResponse,
        cache: CacheStatus,
    },
    NotFound,
    OfflineCacheMiss,
    Cancelled,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LyricsServiceError {
    InvalidTrack,
    InvalidResponse,
    NoMatchingTrack,
    Network,
    HttpStatus(u16),
    CacheIo,
}

impl std::fmt::Display for LyricsServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTrack => formatter.write_str("Track metadata is invalid."),
            Self::InvalidResponse => {
                formatter.write_str("The lyrics provider returned invalid data.")
            }
            Self::NoMatchingTrack => formatter.write_str("No matching lyrics were returned."),
            Self::Network => formatter.write_str("The lyrics provider could not be reached."),
            Self::HttpStatus(status) => {
                write!(formatter, "The lyrics provider returned HTTP {status}.")
            }
            Self::CacheIo => formatter.write_str("The local lyrics cache could not be accessed."),
        }
    }
}

impl std::error::Error for LyricsServiceError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Network,
    Cancelled,
}

/// Injectable boundary for the single public LRCLIB GET endpoint.
pub trait LyricsTransport: Send + Sync + 'static {
    fn get(
        &self,
        url: &str,
        cancellation: &CancellationToken,
    ) -> Result<TransportResponse, TransportError>;
}

pub trait Clock: Send + Sync + 'static {
    fn now_unix_seconds(&self) -> u64;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct LookupRequest {
    generation: u64,
    track: TrackMetadata,
    cancellation: CancellationToken,
    offline: bool,
}

impl LookupRequest {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub cache_ttl: Duration,
    pub request_timeout: Duration,
    pub max_attempts: u8,
    pub retry_delay: Duration,
    /// Absolute duration allowance, in addition to the relative allowance.
    pub duration_tolerance: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            cache_ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            request_timeout: Duration::from_secs(8),
            max_attempts: 3,
            retry_delay: Duration::from_millis(250),
            duration_tolerance: Duration::from_secs(2),
        }
    }
}

#[derive(Clone)]
pub struct LyricsService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    transport: Arc<dyn LyricsTransport>,
    clock: Arc<dyn Clock>,
    cache_root: PathBuf,
    config: ServiceConfig,
    generation: AtomicU64,
    memory_cache: Mutex<HashMap<String, CacheEntry>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CacheEntry {
    version: u8,
    fetched_at: u64,
    record: ProviderRecord,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRecord {
    track_name: String,
    artist_name: String,
    album_name: Option<String>,
    duration: f64,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    has_word_sync: bool,
    #[serde(default)]
    lyricsfile: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderResponse {
    track_name: String,
    artist_name: String,
    album_name: Option<String>,
    duration: f64,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    has_word_sync: bool,
    #[serde(default)]
    lyricsfile: Option<serde_json::Value>,
}

impl From<ProviderResponse> for ProviderRecord {
    fn from(response: ProviderResponse) -> Self {
        Self {
            track_name: response.track_name,
            artist_name: response.artist_name,
            album_name: response.album_name,
            duration: response.duration,
            plain_lyrics: response.plain_lyrics,
            synced_lyrics: response.synced_lyrics,
            instrumental: response.instrumental,
            has_word_sync: response.has_word_sync,
            lyricsfile: response.lyricsfile,
        }
    }
}

impl LyricsService {
    /// Construct with explicit, app-owned cache storage. No global cache paths are selected.
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        let config = ServiceConfig::default();
        Self::with_components(
            cache_root,
            Arc::new(LrclibTransport {
                timeout: config.request_timeout,
            }),
            Arc::new(SystemClock),
            config,
        )
    }

    /// Construct a deterministic service by injecting network, time, storage and retry settings.
    pub fn with_components(
        cache_root: impl Into<PathBuf>,
        transport: Arc<dyn LyricsTransport>,
        clock: Arc<dyn Clock>,
        mut config: ServiceConfig,
    ) -> Self {
        if config.max_attempts == 0 {
            config.max_attempts = 1;
        }
        Self {
            inner: Arc::new(ServiceInner {
                transport,
                clock,
                cache_root: cache_root.into(),
                config,
                generation: AtomicU64::new(0),
                memory_cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Starting a newer request makes any older result stale, even if it finishes later.
    pub fn begin_lookup(&self, track: TrackMetadata) -> LookupRequest {
        self.begin_request(track, false)
    }

    /// Begin a cache-only lookup. Expired matching entries are returned as stale offline data.
    pub fn begin_offline_lookup(&self, track: TrackMetadata) -> LookupRequest {
        self.begin_request(track, true)
    }

    fn begin_request(&self, track: TrackMetadata, offline: bool) -> LookupRequest {
        LookupRequest {
            generation: self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1,
            track,
            cancellation: CancellationToken::default(),
            offline,
        }
    }

    pub fn lookup(&self, request: LookupRequest) -> Result<LookupOutcome, LyricsServiceError> {
        request.track.validate()?;
        if let Some(outcome) = self.request_state(&request) {
            return Ok(outcome);
        }

        let key = cache_key(&request.track);
        let mut stale = None;
        if let Some((entry, location)) = self.load_cached(&key)? {
            if record_matches(&entry.record, &request.track, &self.inner.config) {
                let fresh = self.cache_is_fresh(&entry);
                let parsed = parse_provider_record(&entry.record)?;
                if fresh {
                    if let Some(outcome) = self.request_state(&request) {
                        return Ok(outcome);
                    }
                    return Ok(LookupOutcome::Found {
                        response: parsed,
                        cache: location,
                    });
                }
                stale = Some((parsed, location));
            }
        }

        if request.offline {
            if let Some((response, _)) = stale {
                if let Some(outcome) = self.request_state(&request) {
                    return Ok(outcome);
                }
                return Ok(LookupOutcome::Found {
                    response,
                    cache: CacheStatus::StaleOffline,
                });
            }
            return Ok(LookupOutcome::OfflineCacheMiss);
        }

        let url = build_lookup_url(&request.track);
        let attempts = self.inner.config.max_attempts.clamp(1, MAX_RETRY_ATTEMPTS);
        for attempt in 0..attempts {
            if let Some(outcome) = self.request_state(&request) {
                return Ok(outcome);
            }

            match self.inner.transport.get(&url, &request.cancellation) {
                Err(TransportError::Cancelled) => return Ok(LookupOutcome::Cancelled),
                Err(TransportError::Network) => {
                    if attempt + 1 < attempts {
                        self.wait_before_retry(&request, attempt);
                        continue;
                    }
                    if let Some((response, _)) = stale {
                        if let Some(outcome) = self.request_state(&request) {
                            return Ok(outcome);
                        }
                        return Ok(LookupOutcome::Found {
                            response,
                            cache: CacheStatus::StaleOffline,
                        });
                    }
                    return Err(LyricsServiceError::Network);
                }
                Ok(response) if response.status == 404 => return Ok(LookupOutcome::NotFound),
                Ok(response) if is_transient_status(response.status) => {
                    if attempt + 1 < attempts {
                        self.wait_before_retry(&request, attempt);
                        continue;
                    }
                    if let Some((response, _)) = stale {
                        if let Some(outcome) = self.request_state(&request) {
                            return Ok(outcome);
                        }
                        return Ok(LookupOutcome::Found {
                            response,
                            cache: CacheStatus::StaleOffline,
                        });
                    }
                    return Err(LyricsServiceError::HttpStatus(response.status));
                }
                Ok(response) if !(200..300).contains(&response.status) => {
                    return Err(LyricsServiceError::HttpStatus(response.status));
                }
                Ok(response) => {
                    if response.body.len() > MAX_RESPONSE_BYTES {
                        return Err(LyricsServiceError::InvalidResponse);
                    }
                    let provider: ProviderResponse = serde_json::from_slice(&response.body)
                        .map_err(|_| LyricsServiceError::InvalidResponse)?;
                    let record = ProviderRecord::from(provider);
                    validate_record(&record)?;
                    if !record_matches(&record, &request.track, &self.inner.config) {
                        return Err(LyricsServiceError::NoMatchingTrack);
                    }
                    let parsed = parse_provider_record(&record)?;
                    if let Some(outcome) = self.request_state(&request) {
                        return Ok(outcome);
                    }
                    let entry = CacheEntry {
                        version: CACHE_FORMAT_VERSION,
                        fetched_at: self.inner.clock.now_unix_seconds(),
                        record,
                    };
                    self.store_cached(key, entry)?;
                    if let Some(outcome) = self.request_state(&request) {
                        return Ok(outcome);
                    }
                    return Ok(LookupOutcome::Found {
                        response: parsed,
                        cache: CacheStatus::Network,
                    });
                }
            }
        }
        Err(LyricsServiceError::Network)
    }

    fn request_state(&self, request: &LookupRequest) -> Option<LookupOutcome> {
        if request.cancellation.is_cancelled() {
            Some(LookupOutcome::Cancelled)
        } else if request.generation != self.inner.generation.load(Ordering::Acquire) {
            Some(LookupOutcome::Stale)
        } else {
            None
        }
    }

    fn wait_before_retry(&self, request: &LookupRequest, attempt: u8) {
        let multiplier = 1_u32 << u32::from(attempt.min(5));
        let remaining = self
            .inner
            .config
            .retry_delay
            .checked_mul(multiplier)
            .unwrap_or(Duration::from_secs(30))
            .min(Duration::from_secs(30));
        if remaining.is_zero() {
            return;
        }
        let slice = Duration::from_millis(25).min(remaining);
        let deadline = std::time::Instant::now() + remaining;
        while std::time::Instant::now() < deadline {
            if request.cancellation.is_cancelled()
                || request.generation != self.inner.generation.load(Ordering::Acquire)
            {
                return;
            }
            thread::sleep(slice);
        }
    }

    fn cache_is_fresh(&self, entry: &CacheEntry) -> bool {
        let ttl = self.inner.config.cache_ttl.as_secs();
        self.inner.clock.now_unix_seconds() < entry.fetched_at.saturating_add(ttl)
    }

    fn load_cached(
        &self,
        key: &str,
    ) -> Result<Option<(CacheEntry, CacheStatus)>, LyricsServiceError> {
        if let Some(entry) = self
            .inner
            .memory_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
        {
            if entry.version == CACHE_FORMAT_VERSION {
                return Ok(Some((entry, CacheStatus::Memory)));
            }
        }

        let path = self.inner.cache_root.join(format!("{key}.json"));
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(LyricsServiceError::CacheIo),
        };
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Ok(None);
        }
        let entry: CacheEntry =
            serde_json::from_slice(&bytes).map_err(|_| LyricsServiceError::CacheIo)?;
        if entry.version != CACHE_FORMAT_VERSION {
            return Ok(None);
        }
        self.remember_cache(key.to_owned(), entry.clone());
        Ok(Some((entry, CacheStatus::Disk)))
    }

    fn store_cached(&self, key: String, entry: CacheEntry) -> Result<(), LyricsServiceError> {
        fs::create_dir_all(&self.inner.cache_root).map_err(|_| LyricsServiceError::CacheIo)?;
        let path = self.inner.cache_root.join(format!("{key}.json"));
        let temporary = self.inner.cache_root.join(format!(
            ".{key}.{}.{}.tmp",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_vec(&entry).map_err(|_| LyricsServiceError::CacheIo)?;
        prune_cache_directory(&self.inner.cache_root, &path, bytes.len() as u64)?;
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            match fs::rename(&temporary, &path) {
                Ok(()) => Ok(()),
                Err(_) if path.exists() => {
                    fs::remove_file(&path)?;
                    fs::rename(&temporary, &path)
                }
                Err(error) => Err(error),
            }
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(LyricsServiceError::CacheIo);
        }
        self.remember_cache(key, entry);
        Ok(())
    }

    fn remember_cache(&self, key: String, entry: CacheEntry) {
        let mut cache = self
            .inner
            .memory_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !cache.contains_key(&key) && cache.len() >= MAX_MEMORY_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, entry);
    }
}

fn prune_cache_directory(
    root: &Path,
    incoming: &Path,
    incoming_bytes: u64,
) -> Result<(), LyricsServiceError> {
    let entries = fs::read_dir(root).map_err(|_| LyricsServiceError::CacheIo)?;
    let mut existing = Vec::new();
    let mut count = 1_usize;
    let mut total_bytes = incoming_bytes;
    for entry in entries {
        let entry = entry.map_err(|_| LyricsServiceError::CacheIo)?;
        let path = entry.path();
        if path == incoming
            || !entry
                .file_type()
                .map_err(|_| LyricsServiceError::CacheIo)?
                .is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || !path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    stem.len() == 16 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|_| LyricsServiceError::CacheIo)?;
        count += 1;
        total_bytes = total_bytes.saturating_add(metadata.len());
        existing.push((
            metadata.modified().unwrap_or(UNIX_EPOCH),
            path,
            metadata.len(),
        ));
    }

    existing.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, size) in existing {
        if count <= MAX_DISK_CACHE_ENTRIES && total_bytes <= MAX_DISK_CACHE_BYTES {
            break;
        }
        fs::remove_file(path).map_err(|_| LyricsServiceError::CacheIo)?;
        count -= 1;
        total_bytes = total_bytes.saturating_sub(size);
    }
    Ok(())
}

fn validate_record(record: &ProviderRecord) -> Result<(), LyricsServiceError> {
    if record.track_name.trim().is_empty()
        || record.artist_name.trim().is_empty()
        || record.track_name.chars().count() > MAX_TRACK_FIELD_CHARS
        || record.artist_name.chars().count() > MAX_TRACK_FIELD_CHARS
        || record
            .album_name
            .as_ref()
            .is_some_and(|album| album.chars().count() > MAX_TRACK_FIELD_CHARS)
        || !record.duration.is_finite()
        || !(0.0..=86_400.0).contains(&record.duration)
        || (!record.instrumental
            && record.plain_lyrics.is_none()
            && record.synced_lyrics.is_none()
            && !(record.has_word_sync && record.lyricsfile.is_some()))
        || record
            .plain_lyrics
            .as_ref()
            .is_some_and(|lyrics| lyrics.len() > MAX_RESPONSE_BYTES)
        || record
            .synced_lyrics
            .as_ref()
            .is_some_and(|lyrics| lyrics.len() > MAX_RESPONSE_BYTES)
        || record.lyricsfile.as_ref().is_some_and(|lyricsfile| {
            serde_json::to_vec(lyricsfile)
                .map_or(true, |serialized| serialized.len() > MAX_RESPONSE_BYTES)
        })
    {
        return Err(LyricsServiceError::InvalidResponse);
    }
    Ok(())
}

fn parse_provider_record(record: &ProviderRecord) -> Result<LyricsResponse, LyricsServiceError> {
    validate_record(record)?;
    if record.instrumental {
        return Ok(LyricsResponse {
            lyrics: LyricsResult {
                lines: Vec::new(),
                quality: LyricsQuality::Instrumental,
                message: Some("Instrumental track.".to_owned()),
            },
            source: LyricsSource::Instrumental,
            track_title: record.track_name.clone(),
            artist: record.artist_name.clone(),
        });
    }

    if record.has_word_sync {
        if let Some(parsed) = parse_provider_lyricsfile(record) {
            return Ok(parsed);
        }
    }

    if let Some(synced) = record
        .synced_lyrics
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        let parsed = parse_lrc(synced, duration_ms(record.duration));
        let source = match parsed.quality {
            LyricsQuality::Word => Some(LyricsSource::SynchronizedWords),
            LyricsQuality::Line => Some(LyricsSource::SynchronizedLines),
            LyricsQuality::Mixed => Some(LyricsSource::SynchronizedMixed),
            _ => None,
        };
        if let Some(source) = source {
            return Ok(LyricsResponse {
                lyrics: parsed,
                source,
                track_title: record.track_name.clone(),
                artist: record.artist_name.clone(),
            });
        }
    }

    if let Some(plain) = record
        .plain_lyrics
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        let parsed = parse_plain(plain);
        if parsed.quality == LyricsQuality::Plain {
            return Ok(LyricsResponse {
                lyrics: parsed,
                source: LyricsSource::Unsynchronized,
                track_title: record.track_name.clone(),
                artist: record.artist_name.clone(),
            });
        }
    }
    Err(LyricsServiceError::InvalidResponse)
}

fn parse_provider_lyricsfile(record: &ProviderRecord) -> Option<LyricsResponse> {
    let payload = record.lyricsfile.as_ref()?.as_object()?;
    let version = payload
        .get("version")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String("1.0".to_owned()));
    let lyrics_document = if payload.contains_key("metadata") {
        serde_json::Value::Object(payload.clone())
    } else {
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "title".into(),
            serde_json::Value::String(record.track_name.clone()),
        );
        metadata.insert(
            "artist".into(),
            serde_json::Value::String(record.artist_name.clone()),
        );
        if let Some(album) = &record.album_name {
            metadata.insert("album".into(), serde_json::Value::String(album.clone()));
        }
        if let Some(duration) = duration_ms(record.duration) {
            metadata.insert(
                "duration_ms".into(),
                serde_json::Value::Number(duration.into()),
            );
        }
        serde_json::json!({
            "version": version,
            "metadata": metadata,
            "lines": payload.get("lines").cloned().unwrap_or(serde_json::Value::Null),
            "plain": payload.get("plain").cloned().unwrap_or(serde_json::Value::Null),
        })
    };
    if let Some(metadata) = lyrics_document
        .get("metadata")
        .and_then(serde_json::Value::as_object)
    {
        let title = metadata.get("title").and_then(serde_json::Value::as_str)?;
        let artist = metadata.get("artist").and_then(serde_json::Value::as_str)?;
        if normalize_title(title) != normalize_title(&record.track_name)
            || normalize_artist(artist) != normalize_artist(&record.artist_name)
        {
            return None;
        }
    }
    let yaml = serde_yaml_ng::to_string(&lyrics_document).ok()?;
    let lyrics = parse_lyricsfile(&yaml, duration_ms(record.duration));
    let source = match lyrics.quality {
        LyricsQuality::Word => LyricsSource::SynchronizedWords,
        LyricsQuality::Mixed => LyricsSource::SynchronizedMixed,
        _ => return None,
    };
    Some(LyricsResponse {
        lyrics,
        source,
        track_title: record.track_name.clone(),
        artist: record.artist_name.clone(),
    })
}

fn duration_ms(seconds: f64) -> Option<u64> {
    (seconds > 0.0 && seconds.is_finite()).then(|| (seconds * 1000.0).round() as u64)
}

fn record_matches(record: &ProviderRecord, track: &TrackMetadata, config: &ServiceConfig) -> bool {
    if normalize_title(&record.track_name) != normalize_title(&track.title)
        || normalize_artist(&record.artist_name) != normalize_artist(&track.artist)
    {
        return false;
    }

    if let (Some(expected), Some(actual)) = (
        track.duration_ms.filter(|duration| *duration > 0),
        duration_ms(record.duration),
    ) {
        let tolerance = config.duration_tolerance.as_millis().min(u64::MAX as u128) as u64;
        if expected.abs_diff(actual) > tolerance {
            return false;
        }
    }

    true
}

fn cache_key(track: &TrackMetadata) -> String {
    let identity = format!(
        "{}|{}|{}",
        normalize_title(&track.title),
        normalize_artist(&track.artist),
        track
            .album
            .as_deref()
            .map(normalize_identity)
            .unwrap_or_default()
    );
    // Stable FNV-1a is used only to create a compact, filesystem-safe cache filename.
    let hash = identity.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

fn build_lookup_url(track: &TrackMetadata) -> String {
    let mut params = vec![
        ("track_name", track.title.as_str()),
        ("artist_name", track.artist.as_str()),
    ];
    if let Some(album) = track.album.as_deref() {
        params.push(("album_name", album));
    }
    let duration = track
        .duration_ms
        .filter(|milliseconds| *milliseconds > 0)
        .map(|milliseconds| (milliseconds.saturating_add(500) / 1000).to_string());
    let mut query = params
        .into_iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect::<Vec<_>>();
    if let Some(duration) = duration {
        query.push(format!("duration={duration}"));
    }
    format!("{ENDPOINT}?{}", query.join("&"))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn is_transient_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

struct LrclibTransport {
    timeout: Duration,
}

impl LyricsTransport for LrclibTransport {
    fn get(
        &self,
        url: &str,
        cancellation: &CancellationToken,
    ) -> Result<TransportResponse, TransportError> {
        if cancellation.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.timeout)
            .timeout_read(self.timeout)
            .timeout_write(self.timeout)
            .build();
        let response = match agent
            .get(url)
            .set(
                "User-Agent",
                "LyricMotion/1.0 (public LRCLIB lyrics lookup)",
            )
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(_)) => return Err(TransportError::Network),
        };
        if cancellation.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let status = response.status();
        let mut body = Vec::new();
        response
            .into_reader()
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|_| TransportError::Network)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Ok(TransportResponse {
                status,
                body: vec![0; MAX_RESPONSE_BYTES + 1],
            });
        }
        Ok(TransportResponse { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    const SYNCED: &str = "[00:01.00]First line\n[00:02.00]Second line";

    struct FakeTransport {
        responses: Mutex<VecDeque<Result<TransportResponse, TransportError>>>,
        calls: AtomicUsize,
        on_get: Option<Arc<dyn Fn(&CancellationToken) + Send + Sync>>,
    }

    impl FakeTransport {
        fn new(
            responses: impl IntoIterator<Item = Result<TransportResponse, TransportError>>,
        ) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: AtomicUsize::new(0),
                on_get: None,
            }
        }

        fn with_hook(
            hook: impl Fn(&CancellationToken) + Send + Sync + 'static,
            response: Result<TransportResponse, TransportError>,
        ) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from([response])),
                calls: AtomicUsize::new(0),
                on_get: Some(Arc::new(hook)),
            }
        }
    }

    impl LyricsTransport for FakeTransport {
        fn get(
            &self,
            url: &str,
            cancellation: &CancellationToken,
        ) -> Result<TransportResponse, TransportError> {
            assert!(url.starts_with(ENDPOINT));
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(hook) = &self.on_get {
                hook(cancellation);
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportError::Network))
        }
    }

    #[derive(Default)]
    struct FakeClock(AtomicU64);

    impl Clock for FakeClock {
        fn now_unix_seconds(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    impl FakeClock {
        fn set(&self, seconds: u64) {
            self.0.store(seconds, Ordering::Relaxed);
        }
    }

    struct TestCache(PathBuf);

    impl TestCache {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("target")
                .join("lyrics-service-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TestCache {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn service(
        cache: &TestCache,
        transport: Arc<dyn LyricsTransport>,
        clock: Arc<FakeClock>,
        config: ServiceConfig,
    ) -> LyricsService {
        LyricsService::with_components(&cache.0, transport, clock, config)
    }

    fn config() -> ServiceConfig {
        ServiceConfig {
            retry_delay: Duration::ZERO,
            ..ServiceConfig::default()
        }
    }

    fn track() -> TrackMetadata {
        TrackMetadata {
            title: "Blue Hour (feat. Guest)".to_owned(),
            artist: "Example Artist".to_owned(),
            album: Some("Night Drive".to_owned()),
            duration_ms: Some(180_000),
        }
    }

    fn json(
        title: &str,
        artist: &str,
        duration: f64,
        plain: Option<&str>,
        synced: Option<&str>,
    ) -> TransportResponse {
        let body = serde_json::json!({
            "trackName": title,
            "artistName": artist,
            "albumName": "Night Drive",
            "duration": duration,
            "plainLyrics": plain,
            "syncedLyrics": synced,
            "instrumental": false
        });
        TransportResponse {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn success(synced: Option<&str>) -> Result<TransportResponse, TransportError> {
        success_for("Blue Hour", synced)
    }

    fn success_for(title: &str, synced: Option<&str>) -> Result<TransportResponse, TransportError> {
        Ok(json(
            title,
            "Example Artist",
            181.0,
            Some("Unsynchronized lyric text"),
            synced,
        ))
    }

    #[test]
    fn normalization_and_duration_tolerant_matching_keep_identity_strict() {
        assert_eq!(normalize_title("BÍue-Hour (feat. Guest)"), "biue hour");
        assert_eq!(normalize_artist("Beyoncé & Co."), "beyonce and co");

        let cache = TestCache::new();
        let transport = Arc::new(FakeTransport::new([success(Some(SYNCED))]));
        let service = service(
            &cache,
            transport.clone(),
            Arc::new(FakeClock::default()),
            config(),
        );
        assert!(matches!(
            service.lookup(service.begin_lookup(track())).unwrap(),
            LookupOutcome::Found { .. }
        ));
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn duration_matching_uses_the_two_second_provider_tolerance() {
        let cache = TestCache::new();
        let transport = Arc::new(FakeTransport::new([Ok(json(
            "Blue Hour",
            "Example Artist",
            183.0,
            Some("lyrics"),
            None,
        ))]));
        let service = service(&cache, transport, Arc::new(FakeClock::default()), config());
        assert_eq!(
            service.lookup(service.begin_lookup(track())),
            Err(LyricsServiceError::NoMatchingTrack)
        );
    }

    #[test]
    fn stale_and_cancelled_requests_never_publish_provider_results() {
        let cache = TestCache::new();
        let cancelling_transport = Arc::new(FakeTransport::with_hook(
            |token| token.cancel(),
            success(Some(SYNCED)),
        ));
        let cancelling_service = service(
            &cache,
            cancelling_transport,
            Arc::new(FakeClock::default()),
            config(),
        );
        assert_eq!(
            cancelling_service
                .lookup(cancelling_service.begin_lookup(track()))
                .unwrap(),
            LookupOutcome::Cancelled
        );

        let transport = Arc::new(FakeTransport::new([success(Some(SYNCED))]));
        let service = service(&cache, transport, Arc::new(FakeClock::default()), config());
        let old = service.begin_lookup(track());
        let newer = service.begin_lookup(track());
        assert_eq!(service.lookup(old).unwrap(), LookupOutcome::Stale);
        assert!(matches!(
            service.lookup(newer).unwrap(),
            LookupOutcome::Found { .. }
        ));
    }

    #[test]
    fn older_in_flight_generation_is_suppressed_after_a_newer_request_starts() {
        let cache = TestCache::new();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let entered_hook = entered.clone();
        let release_hook = release.clone();
        let transport = Arc::new(FakeTransport::with_hook(
            move |_| {
                entered_hook.wait();
                release_hook.wait();
            },
            success(Some(SYNCED)),
        ));
        let service = service(&cache, transport, Arc::new(FakeClock::default()), config());
        let old_request = service.begin_lookup(track());
        let worker_service = service.clone();
        let worker = std::thread::spawn(move || worker_service.lookup(old_request).unwrap());
        entered.wait();
        let _newer_request = service.begin_lookup(track());
        release.wait();
        assert_eq!(worker.join().unwrap(), LookupOutcome::Stale);
    }

    #[test]
    fn cache_serves_memory_and_disk_then_expired_entries_offline() {
        let cache = TestCache::new();
        let clock = Arc::new(FakeClock::default());
        clock.set(100);
        let online = Arc::new(FakeTransport::new([success(Some(SYNCED))]));
        let first = service(&cache, online.clone(), clock.clone(), config());
        let track = track();
        assert!(matches!(
            first.lookup(first.begin_lookup(track.clone())).unwrap(),
            LookupOutcome::Found {
                cache: CacheStatus::Network,
                ..
            }
        ));
        assert!(matches!(
            first.lookup(first.begin_lookup(track.clone())).unwrap(),
            LookupOutcome::Found {
                cache: CacheStatus::Memory,
                ..
            }
        ));
        assert_eq!(online.calls.load(Ordering::Relaxed), 1);

        let disk_transport = Arc::new(FakeTransport::new([]));
        let second = service(&cache, disk_transport.clone(), clock.clone(), config());
        assert!(matches!(
            second.lookup(second.begin_lookup(track.clone())).unwrap(),
            LookupOutcome::Found {
                cache: CacheStatus::Disk,
                ..
            }
        ));
        assert_eq!(disk_transport.calls.load(Ordering::Relaxed), 0);

        clock.set(100 + DEFAULT_CACHE_TTL_SECONDS + 1);
        let offline = Arc::new(FakeTransport::new([Err(TransportError::Network)]));
        let third = service(&cache, offline.clone(), clock, config());
        assert!(matches!(
            third.lookup(third.begin_lookup(track)).unwrap(),
            LookupOutcome::Found {
                cache: CacheStatus::StaleOffline,
                ..
            }
        ));
        assert_eq!(offline.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn parser_preserves_word_line_and_unsynchronized_sources() {
        let cache = TestCache::new();
        let transport = Arc::new(FakeTransport::new([
            success(Some("[00:01.00]<00:01.00>Word <00:01.50>timed")),
            success_for("Blue Hour (Live)", Some(SYNCED)),
            success_for("Blue Hour (Acoustic)", None),
        ]));
        let service = service(&cache, transport, Arc::new(FakeClock::default()), config());

        let word = service
            .lookup(service.begin_lookup(track()))
            .expect("lookup succeeds");
        assert!(matches!(
            word,
            LookupOutcome::Found {
                response: LyricsResponse {
                    source: LyricsSource::SynchronizedWords,
                    lyrics: LyricsResult {
                        quality: LyricsQuality::Word,
                        ..
                    },
                    ..
                },
                ..
            }
        ));

        let mut line_track = track();
        line_track.title = "Blue Hour (Live)".to_owned();
        let line = service
            .lookup(service.begin_lookup(line_track))
            .expect("lookup succeeds");
        assert!(matches!(
            line,
            LookupOutcome::Found {
                response: LyricsResponse {
                    source: LyricsSource::SynchronizedLines,
                    lyrics: LyricsResult {
                        quality: LyricsQuality::Line,
                        ..
                    },
                    ..
                },
                ..
            }
        ));

        let mut plain_track = track();
        plain_track.title = "Blue Hour (Acoustic)".to_owned();
        let plain = service
            .lookup(service.begin_lookup(plain_track))
            .expect("lookup succeeds");
        assert!(matches!(
            plain,
            LookupOutcome::Found {
                response: LyricsResponse {
                    source: LyricsSource::Unsynchronized,
                    lyrics: LyricsResult {
                        quality: LyricsQuality::Plain,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn provider_lyricsfile_word_timing_is_used_only_after_matching_and_validation() {
        let cache = TestCache::new();
        let response = serde_json::json!({
            "trackName": "Blue Hour",
            "artistName": "Example Artist",
            "albumName": "Night Drive",
            "duration": 181.0,
            "hasWordSync": true,
            "lyricsfile": {
                "version": "1.0",
                "metadata": {
                    "title": "Blue Hour",
                    "artist": "Example Artist",
                    "duration_ms": 181000
                },
                "lines": [{
                    "text": "Word timed",
                    "start_ms": 1000,
                    "end_ms": 2000,
                    "words": [
                        {"text": "Word ", "start_ms": 1000, "end_ms": 1500},
                        {"text": "timed", "start_ms": 1500, "end_ms": 2000}
                    ]
                }]
            }
        });
        let transport = Arc::new(FakeTransport::new([Ok(TransportResponse {
            status: 200,
            body: serde_json::to_vec(&response).unwrap(),
        })]));
        let service = service(&cache, transport, Arc::new(FakeClock::default()), config());
        let outcome = service.lookup(service.begin_lookup(track())).unwrap();
        assert!(matches!(
            outcome,
            LookupOutcome::Found {
                response: LyricsResponse {
                    source: LyricsSource::SynchronizedWords,
                    lyrics: LyricsResult {
                        quality: LyricsQuality::Word,
                        lines,
                        ..
                    },
                    ..
                },
                ..
            } if lines[0].words.as_ref().is_some_and(|words| words.len() == 2)
        ));
    }

    #[test]
    fn retries_transient_status_then_accepts_a_duration_match() {
        let cache = TestCache::new();
        let transport = Arc::new(FakeTransport::new([
            Ok(TransportResponse {
                status: 503,
                body: Vec::new(),
            }),
            success(Some(SYNCED)),
        ]));
        let service = service(
            &cache,
            transport.clone(),
            Arc::new(FakeClock::default()),
            config(),
        );
        assert!(matches!(
            service.lookup(service.begin_lookup(track())).unwrap(),
            LookupOutcome::Found { .. }
        ));
        assert_eq!(transport.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn explicit_offline_lookup_uses_expired_cache_without_contacting_transport() {
        let cache = TestCache::new();
        let clock = Arc::new(FakeClock::default());
        clock.set(5);
        let online = Arc::new(FakeTransport::new([success(Some(SYNCED))]));
        let first = service(&cache, online, clock.clone(), config());
        first
            .lookup(first.begin_lookup(track()))
            .expect("online lookup");
        clock.set(5 + DEFAULT_CACHE_TTL_SECONDS + 1);

        let offline_transport = Arc::new(FakeTransport::new([]));
        let offline = service(&cache, offline_transport.clone(), clock, config());
        assert!(matches!(
            offline
                .lookup(offline.begin_offline_lookup(track()))
                .unwrap(),
            LookupOutcome::Found {
                cache: CacheStatus::StaleOffline,
                ..
            }
        ));
        assert_eq!(offline_transport.calls.load(Ordering::Relaxed), 0);

        let miss_transport = Arc::new(FakeTransport::new([]));
        let empty_cache = TestCache::new();
        let miss = service(
            &empty_cache,
            miss_transport.clone(),
            Arc::new(FakeClock::default()),
            config(),
        );
        assert_eq!(
            miss.lookup(miss.begin_offline_lookup(track())).unwrap(),
            LookupOutcome::OfflineCacheMiss
        );
        assert_eq!(miss_transport.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rejects_wrong_artist_and_bad_provider_shapes() {
        let cache = TestCache::new();
        let wrong_artist = Arc::new(FakeTransport::new([Ok(json(
            "Blue Hour",
            "Different Artist",
            180.0,
            Some("text"),
            Some(SYNCED),
        ))]));
        let wrong_artist_service = service(
            &cache,
            wrong_artist,
            Arc::new(FakeClock::default()),
            config(),
        );
        assert_eq!(
            wrong_artist_service.lookup(wrong_artist_service.begin_lookup(track())),
            Err(LyricsServiceError::NoMatchingTrack)
        );

        let malformed = Arc::new(FakeTransport::new([Ok(TransportResponse {
            status: 200,
            body: b"{".to_vec(),
        })]));
        let service = service(&cache, malformed, Arc::new(FakeClock::default()), config());
        assert_eq!(
            service.lookup(service.begin_lookup(track())),
            Err(LyricsServiceError::InvalidResponse)
        );
    }
}
