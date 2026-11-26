use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use lyricmotion_core::lyrics_service::{
    CacheStatus, CancellationToken, LookupOutcome, LyricsService, TrackMetadata,
};

use crate::media_bridge;

#[derive(Clone)]
pub(crate) struct LyricsJob {
    pub session_id: String,
    pub epoch: u64,
    pub track_id: String,
    pub metadata: TrackMetadata,
}

struct PendingJob {
    ticket: u64,
    job: LyricsJob,
}

struct ActiveRequest {
    ticket: u64,
    cancellation: CancellationToken,
}

#[derive(Default)]
struct WorkerState {
    latest_ticket: u64,
    pending: Option<PendingJob>,
    active: Option<ActiveRequest>,
    stopped: bool,
}

pub(crate) struct LyricsWorker {
    state: Arc<(Mutex<WorkerState>, Condvar)>,
}

impl LyricsWorker {
    pub(crate) fn new(cache_root: PathBuf) -> Result<Self, std::io::Error> {
        let state = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let worker_state = state.clone();
        thread::Builder::new()
            .name("eonara-lyrics".into())
            .spawn(move || run_worker(LyricsService::new(cache_root), worker_state))?;
        Ok(Self { state })
    }

    pub(crate) fn submit(&self, job: LyricsJob) -> Result<u64, &'static str> {
        let (lock, wake) = &*self.state;
        let mut state = lock
            .lock()
            .map_err(|_| "Lyrics worker state was poisoned")?;
        if state.stopped {
            return Err("Lyrics worker is stopped");
        }
        state.latest_ticket = state.latest_ticket.wrapping_add(1).max(1);
        let ticket = state.latest_ticket;
        if let Some(active) = state.active.take() {
            active.cancellation.cancel();
        }
        state.pending = Some(PendingJob { ticket, job });
        wake.notify_one();
        Ok(ticket)
    }

    pub(crate) fn cancel_current(&self) -> Result<u64, &'static str> {
        let (lock, wake) = &*self.state;
        let mut state = lock
            .lock()
            .map_err(|_| "Lyrics worker state was poisoned")?;
        if state.stopped {
            return Err("Lyrics worker is stopped");
        }
        state.latest_ticket = state.latest_ticket.wrapping_add(1).max(1);
        if let Some(active) = state.active.take() {
            active.cancellation.cancel();
        }
        state.pending = None;
        wake.notify_one();
        Ok(state.latest_ticket)
    }
}

impl Drop for LyricsWorker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.stopped = true;
            state.pending = None;
            if let Some(active) = state.active.take() {
                active.cancellation.cancel();
            }
            wake.notify_one();
        } else {
            log::error!("Unable to stop lyrics worker: state was poisoned");
        }
    }
}

fn run_worker(service: LyricsService, shared: Arc<(Mutex<WorkerState>, Condvar)>) {
    loop {
        let pending = {
            let (lock, wake) = &*shared;
            let Ok(state) = lock.lock() else {
                log::error!("Lyrics worker state was poisoned");
                return;
            };
            let mut state =
                match wake.wait_while(state, |state| state.pending.is_none() && !state.stopped) {
                    Ok(state) => state,
                    Err(_) => {
                        log::error!("Lyrics worker state was poisoned while waiting");
                        return;
                    }
                };
            if state.stopped {
                return;
            }
            state.pending.take()
        };

        let Some(PendingJob { ticket, job }) = pending else {
            continue;
        };
        lookup_for_job(&service, &shared, ticket, job);
    }
}

fn lookup_for_job(
    service: &LyricsService,
    shared: &Arc<(Mutex<WorkerState>, Condvar)>,
    ticket: u64,
    job: LyricsJob,
) {
    let offline_request = service.begin_offline_lookup(job.metadata.clone());
    if !activate_request(shared, ticket, offline_request.cancellation_token()) {
        return;
    }
    let offline_result = service.lookup(offline_request);
    if !is_current(shared, ticket) {
        return;
    }

    let mut stale_cache_displayed = false;
    match offline_result {
        Ok(LookupOutcome::Found { response, cache }) => {
            let should_refresh = cache == CacheStatus::StaleOffline;
            stale_cache_displayed = should_refresh;
            media_bridge::post_lyrics_result(
                job.session_id.clone(),
                job.epoch,
                job.track_id.clone(),
                ticket,
                Ok(LookupOutcome::Found { response, cache }),
            );
            if !should_refresh {
                clear_active(shared, ticket);
                return;
            }
        }
        Ok(LookupOutcome::OfflineCacheMiss) => {}
        Ok(LookupOutcome::Cancelled | LookupOutcome::Stale) => {
            clear_active(shared, ticket);
            return;
        }
        Ok(other) => {
            media_bridge::post_lyrics_result(
                job.session_id.clone(),
                job.epoch,
                job.track_id.clone(),
                ticket,
                Ok(other),
            );
        }
        Err(error) => {
            media_bridge::post_lyrics_result(
                job.session_id.clone(),
                job.epoch,
                job.track_id.clone(),
                ticket,
                Err(error),
            );
        }
    }

    if !is_current(shared, ticket) {
        return;
    }
    let request = service.begin_lookup(job.metadata);
    if !activate_request(shared, ticket, request.cancellation_token()) {
        return;
    }
    let result = service.lookup(request);
    if is_current(shared, ticket) {
        let refresh_has_no_result = stale_cache_displayed
            && match &result {
                Ok(LookupOutcome::NotFound | LookupOutcome::OfflineCacheMiss) | Err(_) => true,
                _ => false,
            };
        if !refresh_has_no_result {
            media_bridge::post_lyrics_result(
                job.session_id,
                job.epoch,
                job.track_id,
                ticket,
                result,
            );
        }
    }
    clear_active(shared, ticket);
}

fn activate_request(
    shared: &Arc<(Mutex<WorkerState>, Condvar)>,
    ticket: u64,
    cancellation: CancellationToken,
) -> bool {
    let (lock, _) = &**shared;
    let Ok(mut state) = lock.lock() else {
        log::error!("Lyrics worker state was poisoned");
        cancellation.cancel();
        return false;
    };
    if state.stopped || state.latest_ticket != ticket {
        cancellation.cancel();
        return false;
    }
    state.active = Some(ActiveRequest {
        ticket,
        cancellation,
    });
    true
}

fn is_current(shared: &Arc<(Mutex<WorkerState>, Condvar)>, ticket: u64) -> bool {
    let (lock, _) = &**shared;
    match lock.lock() {
        Ok(state) => !state.stopped && state.latest_ticket == ticket,
        Err(_) => {
            log::error!("Lyrics worker state was poisoned");
            false
        }
    }
}

fn clear_active(shared: &Arc<(Mutex<WorkerState>, Condvar)>, ticket: u64) {
    let (lock, _) = &**shared;
    match lock.lock() {
        Ok(mut state) => {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.ticket == ticket)
            {
                state.active = None;
            }
        }
        Err(_) => log::error!("Lyrics worker state was poisoned while clearing its request"),
    }
}
