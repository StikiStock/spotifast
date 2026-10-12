//! Deterministic transport fake. No Spotify account, network, audio, timers or
//! random order. It models explicit track lists, not Spotify context resolution.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::broadcast;

use super::{
    Capabilities, CommandKind, EVENT_CAPACITY, PlaybackBackend, PlaybackError, PlaybackEvent,
};
use crate::player::{LocalState, LocalTrack, Playback, PlayerCommand};

#[derive(Default)]
struct MockState {
    state: LocalState,
    manual: VecDeque<LocalTrack>,
    context: VecDeque<LocalTrack>,
    previous: Vec<LocalTrack>,
}

pub struct MockPlaybackBackend {
    capabilities: Capabilities,
    inner: Mutex<MockState>,
    events: broadcast::Sender<PlaybackEvent>,
}

impl Default for MockPlaybackBackend {
    fn default() -> Self {
        Self::new(Capabilities::new(
            CommandKind::ALL
                .into_iter()
                .filter(|kind| *kind != CommandKind::Transfer),
        ))
    }
}

impl MockPlaybackBackend {
    pub fn new(mut capabilities: Capabilities) -> Self {
        // A test cannot accidentally advertise a real Connect handoff.
        capabilities.0.remove(&CommandKind::Transfer);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            capabilities,
            inner: Mutex::default(),
            events,
        }
    }

    pub fn connect(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.state.connected = true;
        inner.state.error = None;
        self.publish(&inner.state);
    }

    /// Advance only when the test asks, rather than reading a wall clock.
    pub fn advance_by(&self, milliseconds: u32) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.state.connected && inner.state.playback == Playback::Playing {
            inner.state.position_ms = inner.state.position_ms.saturating_add(milliseconds);
            self.publish(&inner.state);
        }
    }

    pub fn queued_uris(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .manual
            .iter()
            .chain(&inner.context)
            .map(|track| track.uri.clone())
            .collect()
    }

    fn publish(&self, state: &LocalState) {
        let _ = self
            .events
            .send(PlaybackEvent::State(Box::new(state.clone())));
    }

    fn apply(&self, command: PlayerCommand) -> Result<(), PlaybackError> {
        if !self.capabilities.supports(&command) {
            return Err(PlaybackError::Unsupported(CommandKind::from(&command)));
        }
        super::validate(&command)?;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !inner.state.connected {
            return Err(PlaybackError::Disconnected);
        }
        match command {
            PlayerCommand::Toggle => {
                if inner.state.track.is_none() {
                    return Err(PlaybackError::InvalidRequest("no current track"));
                }
                inner.state.playback = if inner.state.playback == Playback::Playing {
                    Playback::Paused
                } else {
                    Playback::Playing
                };
            }
            PlayerCommand::Pause => inner.state.playback = Playback::Paused,
            PlayerCommand::Resume => {
                if inner.state.track.is_none() {
                    return Err(PlaybackError::InvalidRequest("no current track"));
                }
                inner.state.playback = Playback::Playing;
            }
            PlayerCommand::Load(load) => {
                if load.context_uri.is_some() {
                    return Err(PlaybackError::InvalidRequest(
                        "mock requires explicit track URIs",
                    ));
                }
                let index = load
                    .offset_uri
                    .as_ref()
                    .and_then(|uri| load.uris.iter().position(|item| item == uri))
                    .unwrap_or(load.offset_index.unwrap_or(0) as usize);
                if index >= load.uris.len() {
                    return Err(PlaybackError::InvalidRequest("track offset out of range"));
                }
                let tracks: Vec<_> = load.uris.into_iter().map(track).collect();
                inner.state.track = Some(tracks[index].clone());
                inner.context = tracks.into_iter().skip(index + 1).collect();
                inner.previous.clear();
                inner.state.position_ms = load.position_ms;
                inner.state.playback = if load.play {
                    Playback::Playing
                } else {
                    Playback::Paused
                };
                inner.state.shuffle = load.shuffle.unwrap_or(false);
                inner.state.repeat = load.repeat.unwrap_or(inner.state.repeat);
                inner.state.track_sequence = inner.state.track_sequence.wrapping_add(1);
            }
            PlayerCommand::Next => {
                let next = inner
                    .manual
                    .pop_front()
                    .or_else(|| inner.context.pop_front());
                if let Some(current) = inner.state.track.take() {
                    inner.previous.push(current);
                }
                inner.state.track = next;
                inner.state.position_ms = 0;
                inner.state.track_sequence = inner.state.track_sequence.wrapping_add(1);
                if inner.state.track.is_none() {
                    inner.state.playback = Playback::Stopped;
                }
            }
            PlayerCommand::Previous => {
                if inner.state.position_ms > 3_000 {
                    inner.state.position_ms = 0;
                    inner.state.seek_sequence = inner.state.seek_sequence.wrapping_add(1);
                } else if let Some(previous) = inner.previous.pop() {
                    if let Some(current) = inner.state.track.replace(previous) {
                        inner.context.push_front(current);
                    }
                    inner.state.position_ms = 0;
                    inner.state.track_sequence = inner.state.track_sequence.wrapping_add(1);
                    if inner.state.playback == Playback::Stopped {
                        inner.state.playback = Playback::Playing;
                    }
                }
            }
            PlayerCommand::ClearQueue => inner.manual.clear(),
            PlayerCommand::AddToQueue(uri) => inner.manual.push_back(track(uri)),
            PlayerCommand::Seek(position) => {
                inner.state.position_ms = position;
                inner.state.seek_sequence = inner.state.seek_sequence.wrapping_add(1);
            }
            PlayerCommand::Volume(volume) | PlayerCommand::VolumePreview(volume) => {
                inner.state.volume = volume;
            }
            PlayerCommand::Shuffle(shuffle) => inner.state.shuffle = shuffle,
            PlayerCommand::Repeat(repeat) => inner.state.repeat = repeat,
            PlayerCommand::Transfer => {
                return Err(PlaybackError::Unsupported(CommandKind::Transfer));
            }
        }
        self.publish(&inner.state);
        Ok(())
    }
}

fn track(uri: String) -> LocalTrack {
    LocalTrack {
        uri,
        ..LocalTrack::default()
    }
}

impl PlaybackBackend for MockPlaybackBackend {
    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    fn execute(&self, command: PlayerCommand) -> Result<(), PlaybackError> {
        let result = self.apply(command);
        if let Err(error) = &result {
            let _ = self.events.send(PlaybackEvent::Error(error.clone()));
        }
        result
    }

    fn current_state(&self) -> LocalState {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PlaybackEvent> {
        self.events.subscribe()
    }

    fn disconnect(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !inner.state.connected {
            return;
        }
        inner.state.connected = false;
        inner.state.playback = Playback::Stopped;
        inner.state.position_at = None;
        self.publish(&inner.state);
        let _ = self.events.send(PlaybackEvent::Disconnected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::{LoadSpec, RepeatMode};

    fn load(uris: &[&str]) -> PlayerCommand {
        PlayerCommand::Load(LoadSpec {
            uris: uris.iter().map(|uri| (*uri).into()).collect(),
            play: true,
            ..LoadSpec::default()
        })
    }

    #[test]
    fn transport_uses_the_same_object_safe_contract_and_an_explicit_clock() {
        let mock = MockPlaybackBackend::default();
        mock.connect();
        let backend: &dyn PlaybackBackend = &mock;
        let mut events = backend.subscribe_events();
        backend
            .execute(load(&["spotify:track:a", "spotify:track:b"]))
            .unwrap();
        mock.advance_by(500);
        backend.execute(PlayerCommand::Pause).unwrap();
        mock.advance_by(1_000);
        assert_eq!(backend.current_state().position_ms, 500);
        backend.execute(PlayerCommand::Resume).unwrap();
        mock.advance_by(300);
        assert_eq!(backend.current_state().position_ms, 800);
        assert!(backend.current_state().position_at.is_none());
        assert!(matches!(events.try_recv(), Ok(PlaybackEvent::State(_))));
        backend.execute(PlayerCommand::Seek(4_000)).unwrap();
        backend.execute(PlayerCommand::Previous).unwrap();
        assert_eq!(backend.current_state().position_ms, 0);
        backend.execute(PlayerCommand::Next).unwrap();
        assert_eq!(
            backend.current_state().track.unwrap().uri,
            "spotify:track:b"
        );
        backend.execute(PlayerCommand::Previous).unwrap();
        assert_eq!(
            backend.current_state().track.unwrap().uri,
            "spotify:track:a"
        );
    }

    #[test]
    fn restricted_commands_and_invalid_loads_do_not_mutate_state() {
        let mock =
            MockPlaybackBackend::new(Capabilities::new([CommandKind::Pause, CommandKind::Load]));
        assert_eq!(
            mock.execute(PlayerCommand::Pause),
            Err(PlaybackError::Disconnected)
        );
        mock.connect();
        let mut events = mock.subscribe_events();
        let before = mock.current_state();
        assert!(!mock.capabilities().supports(&PlayerCommand::Next));
        assert_eq!(
            mock.execute(PlayerCommand::Next),
            Err(PlaybackError::Unsupported(CommandKind::Next))
        );
        assert!(matches!(
            events.try_recv(),
            Ok(PlaybackEvent::Error(PlaybackError::Unsupported(
                CommandKind::Next
            )))
        ));
        assert!(mock.execute(load(&[])).is_err());
        assert_eq!(mock.current_state(), before);
    }

    #[test]
    fn manual_duplicates_precede_context_and_clear_keeps_context() {
        let mock = MockPlaybackBackend::default();
        mock.connect();
        mock.execute(load(&["a", "b", "c"])).unwrap();
        for _ in 0..2 {
            mock.execute(PlayerCommand::AddToQueue("x".into())).unwrap();
        }
        assert_eq!(mock.queued_uris(), ["x", "x", "b", "c"]);
        mock.execute(PlayerCommand::Next).unwrap();
        assert_eq!(mock.queued_uris(), ["x", "b", "c"]);
        mock.execute(load(&["d", "e"])).unwrap();
        assert_eq!(mock.queued_uris(), ["x", "e"]);
        mock.execute(PlayerCommand::ClearQueue).unwrap();
        assert_eq!(mock.queued_uris(), ["e"]);
    }

    #[test]
    fn disconnect_is_idempotent_and_lagging_subscribers_can_read_the_snapshot() {
        let mock = MockPlaybackBackend::default();
        mock.connect();
        let mut events = mock.subscribe_events();
        for volume in 0..100 {
            mock.execute(PlayerCommand::VolumePreview(volume)).unwrap();
        }
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        assert_eq!(mock.current_state().volume, 99);
        let mut fresh = mock.subscribe_events();
        mock.disconnect();
        mock.disconnect();
        assert!(matches!(fresh.try_recv(), Ok(PlaybackEvent::State(_))));
        assert_eq!(fresh.try_recv().unwrap(), PlaybackEvent::Disconnected);
        assert!(fresh.try_recv().is_err());
        assert_eq!(
            mock.execute(PlayerCommand::Pause),
            Err(PlaybackError::Disconnected)
        );
    }

    #[test]
    fn transport_preserves_repeat_and_volume_preview_is_distinct() {
        let mock = MockPlaybackBackend::default();
        mock.connect();
        mock.execute(PlayerCommand::Repeat(RepeatMode::Track))
            .unwrap();
        mock.execute(load(&["a"])).unwrap();
        assert_eq!(mock.current_state().repeat, RepeatMode::Track);
        mock.execute(PlayerCommand::Volume(32_000)).unwrap();
        assert_eq!(mock.current_state().volume, 32_000);
        let only_volume = Capabilities::new([CommandKind::Volume]);
        assert!(!only_volume.supports(&PlayerCommand::VolumePreview(0)));
    }
}
