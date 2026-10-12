//! Retains the original engine's authentication, queue, output and recovery.

use std::ops::Deref;
use std::sync::{Arc, Mutex};

use librespot_core::{authentication::Credentials, cache::Cache};
use tokio::sync::broadcast;

use super::{
    Capabilities, CommandKind, EVENT_CAPACITY, PlaybackBackend, PlaybackError, PlaybackEvent,
};
use crate::player::{Engine, EngineConfig, EngineEvent, LocalState, Notify, PlayerCommand};

pub struct LibrespotBackend {
    engine: Engine,
    events: broadcast::Sender<PlaybackEvent>,
}

#[derive(Default)]
struct RelayState {
    connected: bool,
    error: Option<String>,
}

struct EventRelay {
    events: broadcast::Sender<PlaybackEvent>,
    state: Mutex<RelayState>,
}

impl EventRelay {
    fn publish(&self, event: &EngineEvent) {
        let mut previous = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match event {
            EngineEvent::State(state) => {
                // The ordinary UI uses the existing Notify callback. Avoid
                // allocating duplicate metadata when nobody subscribes here.
                if self.events.receiver_count() > 0 {
                    let _ = self
                        .events
                        .send(PlaybackEvent::State(Box::new(state.clone())));
                }
                if state.error != previous.error {
                    if let Some(message) = &state.error {
                        let _ = self
                            .events
                            .send(PlaybackEvent::Error(PlaybackError::Backend(
                                message.clone(),
                            )));
                    }
                    previous.error.clone_from(&state.error);
                }
                if previous.connected && !state.connected {
                    let _ = self.events.send(PlaybackEvent::Disconnected);
                }
                previous.connected = state.connected;
            }
            EngineEvent::SessionEnded => {
                if previous.connected {
                    let _ = self.events.send(PlaybackEvent::Disconnected);
                    previous.connected = false;
                }
            }
        }
    }
}

impl LibrespotBackend {
    /// Backend-specific async construction keeps credentials out of the
    /// transport trait. The existing notify path still wakes egui and handles
    /// credential leases/reconnection exactly as before.
    pub async fn connect(
        config: &EngineConfig,
        proxy: Option<reqwest::Url>,
        credentials: Credentials,
        cache: Cache,
        notify: Notify,
    ) -> anyhow::Result<Self> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let relay = EventRelay {
            events: events.clone(),
            state: Mutex::default(),
        };
        let forward = Arc::new(move |event: EngineEvent| {
            relay.publish(&event);
            notify(event);
        });
        let engine = Engine::connect(config, proxy, credentials, cache, forward).await?;
        Ok(Self { engine, events })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_errors_are_not_repeated_on_every_position_update_and_shutdown_is_reported_once() {
        let (events, mut receive) = broadcast::channel(EVENT_CAPACITY);
        let relay = EventRelay {
            events,
            state: Mutex::default(),
        };
        let mut state = LocalState {
            connected: true,
            ..LocalState::default()
        };
        relay.publish(&EngineEvent::State(state.clone()));
        assert!(matches!(receive.try_recv(), Ok(PlaybackEvent::State(_))));
        state.error = Some("Spotify refused the audio key. Try again later".into());
        relay.publish(&EngineEvent::State(state.clone()));
        assert!(matches!(receive.try_recv(), Ok(PlaybackEvent::State(_))));
        assert!(matches!(receive.try_recv(), Ok(PlaybackEvent::Error(_))));
        state.position_ms = 1_000;
        relay.publish(&EngineEvent::State(state.clone()));
        assert!(matches!(receive.try_recv(), Ok(PlaybackEvent::State(_))));
        assert!(receive.try_recv().is_err());
        state.connected = false;
        relay.publish(&EngineEvent::State(state));
        relay.publish(&EngineEvent::SessionEnded);
        assert!(matches!(receive.try_recv(), Ok(PlaybackEvent::State(_))));
        assert_eq!(receive.try_recv().unwrap(), PlaybackEvent::Disconnected);
        assert!(receive.try_recv().is_err());
    }
}

// The worker also performs librespot-specific catalogue reads and recovery.
// Preserve those APIs without forcing unrelated providers to expose a Session.
// Transport dispatch in the worker uses PlaybackBackend::execute explicitly.
impl Deref for LibrespotBackend {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        &self.engine
    }
}

impl PlaybackBackend for LibrespotBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::new(CommandKind::ALL)
    }

    fn execute(&self, command: PlayerCommand) -> Result<(), PlaybackError> {
        let kind = CommandKind::from(&command);
        let result = super::validate(&command).and_then(|()| {
            self.engine
                .command(command)
                .map_err(|error| PlaybackError::CommandFailed {
                    command: kind,
                    message: error.to_string(),
                })
        });
        if let Err(error) = &result {
            let _ = self.events.send(PlaybackEvent::Error(error.clone()));
        }
        result
    }

    fn current_state(&self) -> LocalState {
        self.engine.current_state()
    }

    fn subscribe_events(&self) -> broadcast::Receiver<PlaybackEvent> {
        self.events.subscribe()
    }

    fn disconnect(&self) {
        self.engine.shutdown();
    }
}
