//! Playback transport boundary. Catalogue reads and credentials belong to the
//! provider's connection setup, not to the interface's transport controls.
//!
//! Connect with the provider's constructor on the runtime, subscribe, then read
//! `current_state`. Commands must enqueue work without waiting for network I/O.
//! Events are bounded; after `RecvError::Lagged`, read a fresh snapshot. A
//! successful command means acceptance, not completed or audible playback.

use std::collections::BTreeSet;

use tokio::sync::broadcast;

use crate::player::{LocalState, PlayerCommand};

mod librespot;
mod mock;
pub mod session;
#[cfg(windows)]
mod windows;

pub use librespot::LibrespotBackend;
pub use mock::MockPlaybackBackend;

/// A command the adapter can accept. This describes transport support, not
/// account entitlement, content availability or successful audio decryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandKind {
    Toggle,
    Pause,
    Resume,
    Next,
    Previous,
    ClearQueue,
    AddToQueue,
    Seek,
    Volume,
    VolumePreview,
    Shuffle,
    Repeat,
    Load,
    Transfer,
}

impl CommandKind {
    pub const ALL: [Self; 14] = [
        Self::Toggle,
        Self::Pause,
        Self::Resume,
        Self::Next,
        Self::Previous,
        Self::ClearQueue,
        Self::AddToQueue,
        Self::Seek,
        Self::Volume,
        Self::VolumePreview,
        Self::Shuffle,
        Self::Repeat,
        Self::Load,
        Self::Transfer,
    ];
}

impl From<&PlayerCommand> for CommandKind {
    fn from(command: &PlayerCommand) -> Self {
        match command {
            PlayerCommand::Toggle => Self::Toggle,
            PlayerCommand::Pause => Self::Pause,
            PlayerCommand::Resume => Self::Resume,
            PlayerCommand::Next => Self::Next,
            PlayerCommand::Previous => Self::Previous,
            PlayerCommand::ClearQueue => Self::ClearQueue,
            PlayerCommand::AddToQueue(_) => Self::AddToQueue,
            PlayerCommand::Seek(_) => Self::Seek,
            PlayerCommand::Volume(_) => Self::Volume,
            PlayerCommand::VolumePreview(_) => Self::VolumePreview,
            PlayerCommand::Shuffle(_) => Self::Shuffle,
            PlayerCommand::Repeat(_) => Self::Repeat,
            PlayerCommand::Load(_) => Self::Load,
            PlayerCommand::Transfer => Self::Transfer,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(BTreeSet<CommandKind>);

impl Capabilities {
    pub fn new(commands: impl IntoIterator<Item = CommandKind>) -> Self {
        Self(commands.into_iter().collect())
    }

    pub fn supports(&self, command: &PlayerCommand) -> bool {
        self.0.contains(&CommandKind::from(command))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlaybackError {
    #[error("playback backend is disconnected")]
    Disconnected,
    #[error("playback backend does not support {0:?}")]
    Unsupported(CommandKind),
    #[error("invalid playback request: {0}")]
    InvalidRequest(&'static str),
    #[error("{command:?} failed: {message}")]
    CommandFailed {
        command: CommandKind,
        message: String,
    },
    #[error("{0}")]
    Backend(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackEvent {
    State(Box<LocalState>),
    Disconnected,
    Error(PlaybackError),
}

/// Object-safe interface for a connected provider. New providers can accept a
/// subset of `PlayerCommand`; never silently translate an unsupported load,
/// queue edit or volume preview into a different operation.
pub trait PlaybackBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn execute(&self, command: PlayerCommand) -> Result<(), PlaybackError>;
    fn current_state(&self) -> LocalState;
    fn subscribe_events(&self) -> broadcast::Receiver<PlaybackEvent>;
    fn disconnect(&self);
}

const EVENT_CAPACITY: usize = 32;

fn validate(command: &PlayerCommand) -> Result<(), PlaybackError> {
    match command {
        PlayerCommand::Load(load) if load.context_uri.is_none() && load.uris.is_empty() => {
            Err(PlaybackError::InvalidRequest("nothing to play"))
        }
        _ => Ok(()),
    }
}
