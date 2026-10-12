//! Companion transport actor, independent of WinRT so real state transitions
//! can be tested with mock sessions on every platform. No audio or credentials.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, watch};

use super::{Capabilities, CommandKind, PlaybackBackend, PlaybackError, PlaybackEvent};
use crate::player::{LocalState, LocalTrack, Playback, PlayerCommand};

#[derive(Clone, Debug, Default)]
pub struct SessionUpdate {
    pub state: LocalState,
    pub capabilities: Capabilities,
    /// Only one bounded cover is retained. No filesystem or network fetch.
    pub artwork: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub identity: u64,
    pub track: Option<LocalTrack>,
    pub playback: Playback,
    pub position_ms: u32,
    pub capabilities: Capabilities,
    pub artwork: Option<Arc<[u8]>>,
}

/// Implementations re-enumerate and verify identity/capabilities before every
/// operation, so a disappearing session never redirects to another app.
pub trait SessionHost: Send + 'static {
    fn observe(&mut self) -> impl Future<Output = Result<Option<SessionSnapshot>, PlaybackError>>;
    fn control(
        &mut self,
        identity: u64,
        command: PlayerCommand,
    ) -> impl Future<Output = Result<bool, PlaybackError>>;
}

pub type SessionNotify = Arc<dyn Fn(SessionUpdate) + Send + Sync>;

/// OS flags are runtime availability, not proof of verified Spotify support.
#[derive(Clone, Copy, Default)]
pub struct SessionControls {
    pub play: bool,
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
    pub seek: bool,
}

impl SessionControls {
    pub fn capabilities(self, playing: bool, test_controls: bool) -> Capabilities {
        let mut allowed = Vec::new();
        if self.play {
            allowed.push(CommandKind::Resume);
        }
        if self.pause {
            allowed.push(CommandKind::Pause);
        }
        if (playing && self.pause) || (!playing && self.play) {
            allowed.push(CommandKind::Toggle);
        }
        if self.next {
            allowed.push(CommandKind::Next);
        }
        if test_controls && self.previous {
            allowed.push(CommandKind::Previous);
        }
        if test_controls && self.seek {
            allowed.push(CommandKind::Seek);
        }
        Capabilities::new(allowed)
    }
}

struct Shared {
    update: Mutex<SessionUpdate>,
    events: broadcast::Sender<PlaybackEvent>,
    notify: SessionNotify,
}

impl Shared {
    fn publish(&self, update: SessionUpdate) {
        *self.update.lock().unwrap_or_else(|p| p.into_inner()) = update.clone();
        let _ = self
            .events
            .send(PlaybackEvent::State(Box::new(update.state.clone())));
        (self.notify)(update);
    }
}

/// Unknown duration is not a zero-length song. Preserve position interpolation
/// without changing librespot's existing LocalState clamping behavior.
pub fn position_now(state: &LocalState) -> u32 {
    if state.playback == Playback::Playing
        && state
            .track
            .as_ref()
            .is_some_and(|track| track.duration_ms == 0)
    {
        state
            .position_ms
            .saturating_add(state.position_at.map_or(0, |at| {
                at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
            }))
    } else {
        state.position_now()
    }
}

pub struct WindowsSessionBackend {
    shared: Arc<Shared>,
    commands: mpsc::Sender<PlayerCommand>,
    stop: watch::Sender<bool>,
}

impl WindowsSessionBackend {
    /// The native factory initializes WinRT on this dedicated thread. The UI
    /// only owns channels and cached snapshots, never Windows API objects.
    pub fn start<H: SessionHost>(
        factory: impl FnOnce(watch::Sender<()>) -> Result<H, PlaybackError> + Send + 'static,
        notify: SessionNotify,
    ) -> Result<Self, PlaybackError> {
        let (commands, receiver) = mpsc::channel(16);
        let (stop, stopping) = watch::channel(false);
        let (dirty, changed) = watch::channel(());
        let (events, _) = broadcast::channel(super::EVENT_CAPACITY);
        let shared = Arc::new(Shared {
            update: Mutex::default(),
            events,
            notify,
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("spotifast-spotify-session".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build();
                let host = factory(dirty);
                match (runtime, host) {
                    (Ok(runtime), Ok(host)) => {
                        runtime.block_on(run(host, worker, receiver, stopping, changed))
                    }
                    _ => unavailable(
                        &worker,
                        Some("Windows media-session access is unavailable".into()),
                    ),
                }
            })
            .map_err(|_| PlaybackError::Backend("Unable to start Windows session worker".into()))?;
        Ok(Self {
            shared,
            commands,
            stop,
        })
    }

    #[cfg(windows)]
    pub fn connect(test_controls: bool, notify: SessionNotify) -> Result<Self, PlaybackError> {
        Self::start(
            move |dirty| super::windows::NativeHost::new(test_controls, dirty),
            notify,
        )
    }
}

impl PlaybackBackend for WindowsSessionBackend {
    fn capabilities(&self) -> Capabilities {
        self.shared
            .update
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .capabilities
            .clone()
    }

    fn execute(&self, command: PlayerCommand) -> Result<(), PlaybackError> {
        if *self.stop.borrow() || !self.current_state().connected {
            return Err(PlaybackError::Disconnected);
        }
        if !self.capabilities().supports(&command) {
            return Err(PlaybackError::Unsupported(CommandKind::from(&command)));
        }
        self.commands.try_send(command).map_err(|_| {
            PlaybackError::Backend("Session command worker is unavailable or busy".into())
        })
    }

    fn current_state(&self) -> LocalState {
        self.shared
            .update
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PlaybackEvent> {
        self.shared.events.subscribe()
    }
    fn disconnect(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for WindowsSessionBackend {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn unavailable(shared: &Shared, error: Option<String>) {
    let was_connected = shared
        .update
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .state
        .connected;
    shared.publish(SessionUpdate {
        state: LocalState {
            error,
            ..LocalState::default()
        },
        ..SessionUpdate::default()
    });
    if was_connected {
        let _ = shared.events.send(PlaybackEvent::Disconnected);
    }
}

#[derive(Default)]
struct Reconcile {
    identity: Option<u64>,
    sequence: u64,
    intent: Option<(Playback, Instant)>,
}

impl Reconcile {
    fn update(&mut self, snapshot: SessionSnapshot, previous: &LocalState) -> SessionUpdate {
        if self.identity != Some(snapshot.identity) || snapshot.track != previous.track {
            self.sequence = self.sequence.wrapping_add(1);
            self.intent = None;
        }
        self.identity = Some(snapshot.identity);
        let mut playback = snapshot.playback;
        if let Some((wanted, at)) = self.intent {
            if snapshot.playback == wanted || at.elapsed() > Duration::from_secs(5) {
                self.intent = None;
            } else {
                playback = wanted;
            }
        }
        SessionUpdate {
            state: LocalState {
                connected: true,
                track: snapshot.track,
                playback,
                position_ms: snapshot.position_ms,
                position_at: (playback == Playback::Playing).then(Instant::now),
                track_sequence: self.sequence,
                ..LocalState::default()
            },
            capabilities: snapshot.capabilities,
            artwork: snapshot.artwork,
        }
    }
}

async fn run<H: SessionHost>(
    mut host: H,
    shared: Arc<Shared>,
    mut commands: mpsc::Receiver<PlayerCommand>,
    mut stop: watch::Receiver<bool>,
    mut dirty: watch::Receiver<()>,
) {
    let mut reconcile = Reconcile::default();
    let mut notifications_open = true;
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            break;
        }
        dirty.borrow_and_update();
        let observed = host.observe().await;
        // A notification during an asynchronous metadata/artwork read makes
        // that read stale. Resnapshot without flashing the old track.
        if dirty.has_changed().unwrap_or(false) {
            continue;
        }
        match observed {
            Ok(Some(snapshot)) => {
                let previous = shared
                    .update
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .state
                    .clone();
                shared.publish(reconcile.update(snapshot, &previous));
            }
            Ok(None) => {
                reconcile.identity = None;
                reconcile.intent = None;
                unavailable(&shared, None);
            }
            Err(error) => {
                reconcile.identity = None;
                reconcile.intent = None;
                unavailable(&shared, Some(error.to_string()));
            }
        }
        tokio::select! {
            _ = stop.changed() => break,
            notification = dirty.changed(), if notifications_open => { if notification.is_err() { notifications_open = false; } },
            _ = interval.tick() => {},
            command = commands.recv() => {
                let Some(command) = command else { break; };
                let Some(identity) = reconcile.identity else { continue; };
                // Revalidate capability and the exact session in the host.
                let mut update = shared.update.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if !update.capabilities.supports(&command) { continue; }
                let mut before_command = update.clone();
                let wanted = match command {
                    PlayerCommand::Pause => Some(Playback::Paused),
                    PlayerCommand::Resume => Some(Playback::Playing),
                    PlayerCommand::Toggle => Some(if update.state.playback == Playback::Playing { Playback::Paused } else { Playback::Playing }),
                    _ => None,
                };
                if let Some(playback) = wanted {
                    update.state.position_ms = position_now(&update.state);
                    update.state.position_at = (playback == Playback::Playing).then(Instant::now);
                    update.state.playback = playback;
                    reconcile.intent = Some((playback, Instant::now()));
                    shared.publish(update);
                }
                match host.control(identity, command).await {
                    Ok(true) => {},
                    result => {
                        reconcile.intent = None;
                        let error = result.err().unwrap_or_else(|| PlaybackError::Backend("Spotify declined the media-session command".into()));
                        let _ = shared.events.send(PlaybackEvent::Error(error.clone()));
                        if error == PlaybackError::Disconnected { unavailable(&shared, Some(error.to_string())); }
                        else { before_command.state.error = Some(error.to_string()); shared.publish(before_command); }
                    }
                }
            }
        }
    }
    unavailable(&shared, None);
}

/// Source IDs are an allowlist, not publisher authentication. No substring
/// match and no fallback to the system's current (possibly unrelated) session.
pub fn is_spotify_source(source: &str) -> bool {
    source.eq_ignore_ascii_case("Spotify.exe")
        || source.eq_ignore_ascii_case("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify")
}

pub fn select_unique<T>(
    sessions: impl IntoIterator<Item = (String, T)>,
) -> Result<Option<T>, PlaybackError> {
    let mut found = None;
    for (source, session) in sessions {
        if is_spotify_source(&source) {
            if found.is_some() {
                return Err(PlaybackError::Backend("Multiple Spotify sessions. Close the extra instance before controlling playback.".into()));
            }
            found = Some(session);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(identity: u64, playback: Playback) -> SessionSnapshot {
        SessionSnapshot {
            identity,
            playback,
            track: Some(LocalTrack {
                title: "Song".into(),
                duration_ms: 60_000,
                ..LocalTrack::default()
            }),
            position_ms: 500,
            capabilities: Capabilities::new([
                CommandKind::Pause,
                CommandKind::Resume,
                CommandKind::Toggle,
                CommandKind::Next,
            ]),
            artwork: None,
        }
    }

    #[test]
    fn exact_selection_never_falls_back_and_ambiguity_fails_closed() {
        assert!(!is_spotify_source("NotSpotify.exe"));
        assert_eq!(select_unique([("msedge.exe".into(), 1)]).unwrap(), None);
        assert_eq!(
            select_unique([("msedge.exe".into(), 1), ("Spotify.exe".into(), 2)]).unwrap(),
            Some(2)
        );
        assert!(
            select_unique([
                ("Spotify.exe".into(), 1),
                ("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify".into(), 2)
            ])
            .is_err()
        );
    }

    #[test]
    fn delayed_pause_reports_do_not_flicker_and_restart_clears_old_intent() {
        let mut reconcile = Reconcile::default();
        let first = reconcile.update(snapshot(1, Playback::Playing), &LocalState::default());
        reconcile.intent = Some((Playback::Paused, Instant::now()));
        let held = reconcile.update(snapshot(1, Playback::Playing), &first.state);
        assert_eq!(held.state.playback, Playback::Paused);
        assert!(held.state.position_at.is_none());
        let confirmed = reconcile.update(snapshot(1, Playback::Paused), &held.state);
        assert!(reconcile.intent.is_none());
        let restart = reconcile.update(snapshot(2, Playback::Playing), &confirmed.state);
        assert!(restart.state.track_sequence > confirmed.state.track_sequence);
        assert!(restart.state.track.as_ref().unwrap().uri.is_empty());
    }

    #[test]
    fn unverified_controls_need_opt_in_and_an_advertised_flag() {
        let flags = SessionControls {
            play: true,
            pause: true,
            next: true,
            previous: true,
            seek: false,
        };
        let verified = flags.capabilities(true, false);
        assert!(verified.supports(&PlayerCommand::Next));
        assert!(!verified.supports(&PlayerCommand::Previous));
        let experimental = flags.capabilities(true, true);
        assert!(experimental.supports(&PlayerCommand::Previous));
        assert!(!experimental.supports(&PlayerCommand::Seek(0)));
        assert!(!experimental.supports(&PlayerCommand::Volume(100)));
        assert!(!experimental.supports(&PlayerCommand::Load(crate::player::LoadSpec::default())));
    }

    #[test]
    fn unknown_duration_advances_but_paused_position_stays_fixed() {
        let mut state = LocalState {
            playback: Playback::Playing,
            track: Some(LocalTrack::default()),
            position_ms: 4000,
            position_at: Some(Instant::now() - Duration::from_secs(2)),
            ..LocalState::default()
        };
        assert!(position_now(&state) >= 6000);
        state.playback = Playback::Paused;
        assert_eq!(position_now(&state), 4000);
    }

    struct MockHost {
        seen: Arc<Mutex<Vec<PlayerCommand>>>,
        present: Arc<Mutex<bool>>,
        identity: Arc<Mutex<u64>>,
        accept: bool,
    }
    impl SessionHost for MockHost {
        async fn observe(&mut self) -> Result<Option<SessionSnapshot>, PlaybackError> {
            Ok((*self.present.lock().unwrap())
                .then(|| snapshot(*self.identity.lock().unwrap(), Playback::Playing)))
        }
        async fn control(
            &mut self,
            identity: u64,
            command: PlayerCommand,
        ) -> Result<bool, PlaybackError> {
            if identity != *self.identity.lock().unwrap() || !*self.present.lock().unwrap() {
                return Err(PlaybackError::Disconnected);
            }
            self.seen.lock().unwrap().push(command);
            Ok(self.accept)
        }
    }

    #[tokio::test]
    async fn mock_session_commands_disconnect_and_unsupported_loads() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let present = Arc::new(Mutex::new(true));
        let host = MockHost {
            seen: Arc::clone(&seen),
            present: Arc::clone(&present),
            identity: Arc::new(Mutex::new(1)),
            accept: true,
        };
        let backend = WindowsSessionBackend::start(move |_| Ok(host), Arc::new(|_| {})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                backend.execute(PlayerCommand::Load(crate::player::LoadSpec::default())),
                Err(PlaybackError::Unsupported(CommandKind::Load))
            );
            assert!(!backend.capabilities().supports(&PlayerCommand::Previous));
            assert!(!backend.capabilities().supports(&PlayerCommand::Seek(100)));
            backend.execute(PlayerCommand::Next).unwrap();
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            *present.lock().unwrap() = false;
            let result = backend.execute(PlayerCommand::Pause);
            assert!(result.is_ok() || result == Err(PlaybackError::Disconnected));
            while backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(seen.lock().unwrap().as_slice(), [PlayerCommand::Next]);
        })
        .await
        .unwrap();
        backend.disconnect();
    }

    #[tokio::test]
    async fn declined_command_reports_error_without_flashing_away_metadata() {
        let host = MockHost {
            seen: Arc::default(),
            present: Arc::new(Mutex::new(true)),
            identity: Arc::new(Mutex::new(1)),
            accept: false,
        };
        let backend = WindowsSessionBackend::start(move |_| Ok(host), Arc::new(|_| {})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let mut events = backend.subscribe_events();
            backend.execute(PlayerCommand::Pause).unwrap();
            let mut error = false;
            let mut restored = false;
            while !error || !restored {
                match events.recv().await.unwrap() {
                    PlaybackEvent::State(state) => {
                        assert!(state.connected);
                        assert_eq!(state.track.as_ref().unwrap().title, "Song");
                        if state.error.is_some() {
                            assert_eq!(state.playback, Playback::Playing);
                            restored = true;
                        }
                    }
                    PlaybackEvent::Error(_) => error = true,
                    PlaybackEvent::Disconnected => panic!("A rejected command is not a disconnect"),
                }
            }
        })
        .await
        .unwrap();
        backend.disconnect();
    }

    #[tokio::test]
    async fn notifications_reattach_after_restart_and_shutdown_clears_state() {
        let present = Arc::new(Mutex::new(true));
        let identity = Arc::new(Mutex::new(1));
        let notification = Arc::new(Mutex::new(None::<watch::Sender<()>>));
        let worker_notification = Arc::clone(&notification);
        let host = MockHost {
            seen: Arc::default(),
            present: Arc::clone(&present),
            identity: Arc::clone(&identity),
            accept: true,
        };
        let backend = WindowsSessionBackend::start(
            move |dirty| {
                *worker_notification.lock().unwrap() = Some(dirty);
                Ok(host)
            },
            Arc::new(|_| {}),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let sequence = backend.current_state().track_sequence;
            *present.lock().unwrap() = false;
            notification
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(())
                .unwrap();
            while backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(backend.current_state().track.is_none());
            assert!(!backend.capabilities().supports(&PlayerCommand::Next));
            *identity.lock().unwrap() = 2;
            *present.lock().unwrap() = true;
            notification
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(())
                .unwrap();
            while !backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(backend.current_state().track_sequence > sequence);
            backend.disconnect();
            while backend.current_state().connected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(backend.current_state().track.is_none());
        })
        .await
        .unwrap();
    }
}
