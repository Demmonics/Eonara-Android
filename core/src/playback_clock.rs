const MAX_PLAYBACK_RATE: f64 = 4.0;
const MIN_PLAYBACK_RATE: f64 = 0.05;
const CORRECTION_DURATION_MS: f64 = 320.0;
const SEEK_SMOOTHING_THRESHOLD_MS: f64 = 160.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Buffering,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSnapshot {
    pub session_id: String,
    pub session_epoch: u64,
    pub revision: u64,
    pub track_id: Option<String>,
    pub position_ms: Option<f64>,
    pub duration_ms: Option<f64>,
    pub playback_state: PlaybackState,
    pub playback_rate: Option<f64>,
    pub observed_at_ms: u64,
    pub timeline_valid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateResult {
    Accepted,
    DurationOnly,
    InvalidTimeline,
    StaleSession,
    StaleRevision,
    StaleAnchor,
}

#[derive(Debug, Default)]
pub struct PlaybackClock {
    active_session: Option<String>,
    session_epoch: Option<u64>,
    last_revision: Option<u64>,
    track_id: Option<String>,
    base_position_ms: f64,
    received_at_ms: u64,
    last_wall_ms: u64,
    playback_rate: f64,
    duration_ms: Option<f64>,
    correction_ms: f64,
    last_source_position_ms: Option<f64>,
    valid: bool,
}

impl PlaybackClock {
    pub fn select_session(&mut self, session_id: Option<String>, epoch: u64) -> bool {
        if self.session_epoch.is_some_and(|current| epoch <= current) {
            return false;
        }

        self.clear_timeline();
        self.active_session = session_id;
        self.session_epoch = Some(epoch);
        self.last_revision = None;
        true
    }

    pub fn update(&mut self, snapshot: &PlaybackSnapshot) -> UpdateResult {
        if self.active_session.as_deref() != Some(snapshot.session_id.as_str())
            || self.session_epoch != Some(snapshot.session_epoch)
        {
            return UpdateResult::StaleSession;
        }
        if self
            .last_revision
            .is_some_and(|revision| snapshot.revision <= revision)
        {
            return UpdateResult::StaleRevision;
        }
        self.last_revision = Some(snapshot.revision);

        if self.valid && snapshot.observed_at_ms < self.last_wall_ms {
            return UpdateResult::StaleAnchor;
        }
        if !snapshot.timeline_valid
            || snapshot
                .position_ms
                .is_none_or(|position| !position.is_finite() || position < 0.0)
        {
            self.clear_timeline();
            return UpdateResult::InvalidTimeline;
        }

        let position_ms = snapshot.position_ms.expect("validated playback position");
        let duration_ms = snapshot
            .duration_ms
            .filter(|duration| duration.is_finite() && *duration > 0.0);
        let playback_rate = match snapshot.playback_state {
            PlaybackState::Playing => normalized_rate(snapshot.playback_rate),
            PlaybackState::Paused | PlaybackState::Buffering | PlaybackState::Stopped => 0.0,
        };
        let track_changed = self.track_id != snapshot.track_id;

        if self.valid
            && !track_changed
            && playback_rate == self.playback_rate
            && Some(position_ms) == self.last_source_position_ms
        {
            self.duration_ms = duration_ms;
            return UpdateResult::DurationOnly;
        }

        let predicted_position_ms = self.at(snapshot.observed_at_ms).unwrap_or(position_ms);
        let delta_ms = position_ms - predicted_position_ms;
        let smooth = self.valid
            && !track_changed
            && playback_rate == self.playback_rate
            && playback_rate > 0.0
            && delta_ms.abs() < SEEK_SMOOTHING_THRESHOLD_MS;

        self.base_position_ms = if smooth {
            position_ms - delta_ms
        } else {
            position_ms
        };
        self.correction_ms = if smooth { delta_ms } else { 0.0 };
        self.received_at_ms = snapshot.observed_at_ms;
        self.last_wall_ms = snapshot.observed_at_ms;
        self.playback_rate = playback_rate;
        self.duration_ms = duration_ms;
        self.track_id = snapshot.track_id.clone();
        self.last_source_position_ms = Some(position_ms);
        self.valid = true;

        UpdateResult::Accepted
    }

    pub fn at(&self, elapsed_realtime_ms: u64) -> Option<f64> {
        if !self.valid {
            return None;
        }

        let elapsed_ms = elapsed_realtime_ms.saturating_sub(self.received_at_ms) as f64;
        let correction_elapsed_ms = elapsed_realtime_ms.saturating_sub(self.last_wall_ms) as f64;
        let correction_progress = (correction_elapsed_ms / CORRECTION_DURATION_MS).min(1.0);
        let position_ms = self.base_position_ms
            + elapsed_ms * self.playback_rate
            + self.correction_ms * correction_progress;
        let position_ms = position_ms.max(0.0);

        Some(
            self.duration_ms
                .map_or(position_ms, |duration| position_ms.min(duration)),
        )
    }

    pub fn duration_ms(&self) -> Option<f64> {
        self.duration_ms
    }

    pub fn playback_rate(&self) -> f64 {
        self.playback_rate
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    fn clear_timeline(&mut self) {
        self.track_id = None;
        self.base_position_ms = 0.0;
        self.received_at_ms = 0;
        self.last_wall_ms = 0;
        self.playback_rate = 0.0;
        self.duration_ms = None;
        self.correction_ms = 0.0;
        self.last_source_position_ms = None;
        self.valid = false;
    }
}

fn normalized_rate(rate: Option<f64>) -> f64 {
    match rate {
        Some(rate) if rate.is_finite() && rate != 0.0 => {
            rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
        }
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackClock, PlaybackSnapshot, PlaybackState, UpdateResult};

    fn snapshot(
        revision: u64,
        position_ms: f64,
        observed_at_ms: u64,
        playback_state: PlaybackState,
    ) -> PlaybackSnapshot {
        PlaybackSnapshot {
            session_id: "player-a".into(),
            session_epoch: 1,
            revision,
            track_id: Some("track-1".into()),
            position_ms: Some(position_ms),
            duration_ms: Some(10_000.0),
            playback_state,
            playback_rate: Some(1.0),
            observed_at_ms,
            timeline_valid: true,
        }
    }

    fn assert_position(clock: &PlaybackClock, at_ms: u64, expected_ms: f64) {
        let actual_ms = clock.at(at_ms).expect("clock should be valid");
        assert!(
            (actual_ms - expected_ms).abs() < 0.01,
            "expected {expected_ms} ms, got {actual_ms} ms"
        );
    }

    fn select_player(clock: &mut PlaybackClock) {
        assert!(clock.select_session(Some("player-a".into()), 1));
    }

    #[test]
    fn advances_from_anchor_with_rate_and_clamps_to_duration() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        let mut sample = snapshot(1, 1_000.0, 100, PlaybackState::Playing);
        sample.playback_rate = Some(1.5);
        assert_eq!(clock.update(&sample), UpdateResult::Accepted);
        assert_position(&clock, 300, 1_300.0);

        sample.revision = 2;
        sample.observed_at_ms = 300;
        assert_eq!(clock.update(&sample), UpdateResult::DurationOnly);
        assert_position(&clock, 400, 1_450.0);
        assert_position(&clock, 10_000, 10_000.0);
    }

    #[test]
    fn pause_resume_and_forward_or_backward_seek_reanchor_immediately() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        assert_eq!(
            clock.update(&snapshot(1, 1_000.0, 100, PlaybackState::Playing)),
            UpdateResult::Accepted
        );

        assert_eq!(
            clock.update(&snapshot(2, 6_000.0, 110, PlaybackState::Playing)),
            UpdateResult::Accepted
        );
        assert_position(&clock, 110, 6_000.0);

        assert_eq!(
            clock.update(&snapshot(3, 300.0, 150, PlaybackState::Paused)),
            UpdateResult::Accepted
        );
        assert_position(&clock, 620, 300.0);

        let mut resumed = snapshot(4, 300.0, 650, PlaybackState::Playing);
        resumed.playback_rate = Some(1.0);
        clock.update(&resumed);
        assert_position(&clock, 950, 600.0);
    }

    #[test]
    fn smooths_small_position_corrections_without_accumulating_duplicates() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        clock.update(&snapshot(1, 1_000.0, 100, PlaybackState::Playing));
        assert_position(&clock, 150, 1_050.0);

        clock.update(&snapshot(2, 1_100.0, 150, PlaybackState::Playing));
        assert_position(&clock, 150, 1_050.0);
        assert_position(&clock, 470, 1_420.0);
    }

    #[test]
    fn uses_elapsed_realtime_anchor_for_delayed_callbacks() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        clock.update(&snapshot(1, 5_000.0, 100, PlaybackState::Playing));
        assert_position(&clock, 500, 5_400.0);
    }

    #[test]
    fn buffering_freezes_position_and_unknown_duration_does_not_clamp() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        let mut buffering = snapshot(1, 8_000.0, 100, PlaybackState::Buffering);
        buffering.duration_ms = None;
        assert_eq!(clock.update(&buffering), UpdateResult::Accepted);
        assert_position(&clock, 60_000, 8_000.0);
        assert_eq!(clock.duration_ms(), None);

        let mut playing = snapshot(2, 8_000.0, 60_000, PlaybackState::Playing);
        playing.duration_ms = None;
        playing.playback_rate = Some(2.0);
        clock.update(&playing);
        assert_position(&clock, 61_000, 10_000.0);
    }

    #[test]
    fn normalizes_invalid_and_extreme_playback_rates() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        let mut sample = snapshot(1, 0.0, 100, PlaybackState::Playing);
        sample.playback_rate = Some(f64::NAN);
        clock.update(&sample);
        assert_eq!(clock.playback_rate(), 1.0);

        sample.revision = 2;
        sample.playback_rate = Some(8.0);
        clock.update(&sample);
        assert_eq!(clock.playback_rate(), 4.0);

        sample.revision = 3;
        sample.playback_rate = Some(-2.0);
        clock.update(&sample);
        assert_eq!(clock.playback_rate(), 0.05);
    }

    #[test]
    fn track_change_invalidates_old_correction_and_reanchors() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        clock.update(&snapshot(1, 8_000.0, 100, PlaybackState::Playing));

        let mut next_track = snapshot(2, 0.0, 200, PlaybackState::Playing);
        next_track.track_id = Some("track-2".into());
        clock.update(&next_track);
        assert_position(&clock, 200, 0.0);
        assert_position(&clock, 1_200, 1_000.0);
    }

    #[test]
    fn rejects_old_session_epochs_and_out_of_order_callbacks() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        clock.update(&snapshot(10, 2_000.0, 100, PlaybackState::Playing));

        assert!(clock.select_session(Some("player-b".into()), 2));
        assert_eq!(
            clock.update(&snapshot(11, 9_000.0, 200, PlaybackState::Playing)),
            UpdateResult::StaleSession
        );
        let mut current = snapshot(1, 500.0, 300, PlaybackState::Playing);
        current.session_id = "player-b".into();
        current.session_epoch = 2;
        assert_eq!(clock.update(&current), UpdateResult::Accepted);

        current.revision = 0;
        current.position_ms = Some(8_000.0);
        assert_eq!(clock.update(&current), UpdateResult::StaleRevision);
        assert_position(&clock, 400, 600.0);
    }

    #[test]
    fn invalid_timeline_and_stale_anchor_never_restore_old_position() {
        let mut clock = PlaybackClock::default();
        select_player(&mut clock);
        clock.update(&snapshot(1, 2_000.0, 100, PlaybackState::Playing));

        let mut stale = snapshot(2, 9_000.0, 99, PlaybackState::Playing);
        assert_eq!(clock.update(&stale), UpdateResult::StaleAnchor);
        assert_position(&clock, 200, 2_100.0);

        stale.revision = 3;
        stale.observed_at_ms = 99;
        stale.timeline_valid = false;
        assert_eq!(clock.update(&stale), UpdateResult::StaleAnchor);

        stale.revision = 4;
        stale.observed_at_ms = 101;
        assert_eq!(clock.update(&stale), UpdateResult::InvalidTimeline);
        assert_eq!(clock.at(300), None);

        stale.revision = 5;
        stale.observed_at_ms = 102;
        stale.timeline_valid = true;
        stale.position_ms = Some(f64::NAN);
        assert_eq!(clock.update(&stale), UpdateResult::InvalidTimeline);
        assert!(!clock.is_valid());
    }

    #[test]
    fn demo_and_live_clock_instances_remain_independent() {
        let mut demo = PlaybackClock::default();
        let mut live = PlaybackClock::default();
        demo.select_session(Some("demo".into()), 1);
        live.select_session(Some("live".into()), 1);

        let mut demo_sample = snapshot(1, 100.0, 0, PlaybackState::Playing);
        demo_sample.session_id = "demo".into();
        demo_sample.duration_ms = None;
        demo.update(&demo_sample);

        let mut live_sample = snapshot(1, 4_000.0, 0, PlaybackState::Paused);
        live_sample.session_id = "live".into();
        live.update(&live_sample);

        assert_position(&demo, 1_000, 1_100.0);
        assert_position(&live, 1_000, 4_000.0);
    }
}
