use crate::playback_clock::{PlaybackSnapshot, PlaybackState};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TransportActions(u8);

impl TransportActions {
    pub const PREVIOUS: u8 = 1 << 0;
    pub const PLAY: u8 = 1 << 1;
    pub const PAUSE: u8 = 1 << 2;
    pub const NEXT: u8 = 1 << 3;
    pub const SEEK: u8 = 1 << 4;

    pub fn from_bits(bits: u8) -> Self {
        Self(bits & (Self::PREVIOUS | Self::PLAY | Self::PAUSE | Self::NEXT | Self::SEEK))
    }

    pub fn contains(&self, action: u8) -> bool {
        self.0 & action == action
    }

    pub fn bits(&self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MediaMetadata {
    pub track_id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub artwork_revision: u64,
}

impl MediaMetadata {
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Unknown track")
    }

    pub fn display_artist(&self) -> &str {
        self.artist
            .as_deref()
            .filter(|artist| !artist.trim().is_empty())
            .unwrap_or("Unknown artist")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaSessionSnapshot {
    pub session_id: String,
    pub session_epoch: u64,
    pub revision: u64,
    pub metadata_revision: u64,
    pub metadata: MediaMetadata,
    pub position_ms: Option<f64>,
    pub playback_state: PlaybackState,
    pub playback_rate: Option<f64>,
    pub observed_at_ms: u64,
    pub actions: TransportActions,
}

impl MediaSessionSnapshot {
    pub fn clock_snapshot(&self) -> PlaybackSnapshot {
        let duration_ms = self
            .metadata
            .duration_ms
            .map(|duration| duration as f64)
            .filter(|duration| *duration > 0.0);
        let timeline_valid = self
            .position_ms
            .is_some_and(|position| position.is_finite() && position >= 0.0);
        PlaybackSnapshot {
            session_id: self.session_id.clone(),
            session_epoch: self.session_epoch,
            revision: self.revision,
            track_id: self.metadata.track_id.clone(),
            position_ms: self
                .position_ms
                .filter(|position| position.is_finite() && *position >= 0.0),
            duration_ms,
            playback_state: self.playback_state,
            playback_rate: self.playback_rate,
            observed_at_ms: self.observed_at_ms,
            timeline_valid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaMetadata, MediaSessionSnapshot, TransportActions};
    use crate::playback_clock::PlaybackState;

    #[test]
    fn maps_metadata_clock_anchor_and_supported_actions_without_inventing_timeline() {
        let snapshot = MediaSessionSnapshot {
            session_id: "com.example.player/42".into(),
            session_epoch: 7,
            revision: 12,
            metadata_revision: 4,
            metadata: MediaMetadata {
                track_id: Some("track-9".into()),
                title: Some("Song".into()),
                artist: Some("Artist".into()),
                album: None,
                duration_ms: Some(180_000),
                artwork_revision: 5,
            },
            position_ms: Some(2_500.0),
            playback_state: PlaybackState::Playing,
            playback_rate: Some(1.0),
            observed_at_ms: 90_000,
            actions: TransportActions::from_bits(
                TransportActions::PLAY | TransportActions::PAUSE | 0b1000_0000,
            ),
        };

        let clock = snapshot.clock_snapshot();
        assert_eq!(clock.session_epoch, 7);
        assert_eq!(clock.revision, 12);
        assert_eq!(clock.track_id.as_deref(), Some("track-9"));
        assert_eq!(clock.position_ms, Some(2_500.0));
        assert_eq!(clock.duration_ms, Some(180_000.0));
        assert_eq!(clock.observed_at_ms, 90_000);
        assert!(clock.timeline_valid);
        assert!(snapshot.actions.contains(TransportActions::PLAY));
        assert!(!snapshot.actions.contains(TransportActions::SEEK));
        assert_eq!(
            snapshot.actions.bits(),
            TransportActions::PLAY | TransportActions::PAUSE
        );
    }

    #[test]
    fn missing_invalid_position_and_duration_remain_unknown() {
        let snapshot = MediaSessionSnapshot {
            session_id: "player".into(),
            session_epoch: 1,
            revision: 1,
            metadata_revision: 0,
            metadata: MediaMetadata::default(),
            position_ms: Some(f64::NAN),
            playback_state: PlaybackState::Paused,
            playback_rate: None,
            observed_at_ms: 1,
            actions: TransportActions::default(),
        };
        let clock = snapshot.clock_snapshot();
        assert_eq!(clock.position_ms, None);
        assert_eq!(clock.duration_ms, None);
        assert!(!clock.timeline_valid);
        assert_eq!(snapshot.metadata.display_title(), "Unknown track");
        assert_eq!(snapshot.metadata.display_artist(), "Unknown artist");
    }
}
