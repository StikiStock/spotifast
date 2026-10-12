//! Direct Windows Runtime GSMTC integration. Runs on the companion's MTA
//! thread, never the renderer. Session objects and event tokens stay here.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};
use tokio::sync::watch;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as MediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};

use super::session::{SessionControls, SessionHost, SessionSnapshot, select_unique};
use super::{Capabilities, CommandKind, PlaybackError};
use crate::api::models::ArtistRef;
use crate::player::{LocalTrack, Playback, PlayerCommand};

const DEADLINE: Duration = Duration::from_secs(5);
const MAX_ART: u64 = 8 * 1024 * 1024;

fn api_error(_: windows::core::Error) -> PlaybackError {
    // No raw OS/Spotify authorization response enters logs or UI diagnostics.
    PlaybackError::Backend(
        "Windows media-session operation failed. Spotify may have stopped or restarted.".into(),
    )
}

macro_rules! await_operation {
    ($operation:expr) => {{
        let operation = $operation.map_err(api_error)?;
        match tokio::time::timeout(DEADLINE, operation.clone().into_future()).await {
            Ok(result) => result.map_err(api_error),
            Err(_) => {
                let _ = operation.Cancel();
                Err(PlaybackError::Backend(
                    "Windows media-session operation timed out".into(),
                ))
            }
        }
    }};
}

async fn read_artwork(metadata: &MediaProperties) -> Result<Arc<[u8]>, PlaybackError> {
    let reference = metadata.Thumbnail().map_err(api_error)?;
    let stream = await_operation!(reference.OpenReadAsync())?;
    let size = stream.Size().map_err(api_error)?;
    if size == 0 || size > MAX_ART {
        return Err(PlaybackError::Backend(
            "Spotify artwork is unavailable or too large".into(),
        ));
    }
    let input = stream.GetInputStreamAt(0).map_err(api_error)?;
    let reader = DataReader::CreateDataReader(&input).map_err(api_error)?;
    let loaded = await_operation!(reader.LoadAsync(size as u32))?;
    if u64::from(loaded) != size {
        return Err(PlaybackError::Backend(
            "Spotify artwork was incomplete".into(),
        ));
    }
    let mut bytes = vec![0; loaded as usize];
    reader.ReadBytes(&mut bytes).map_err(api_error)?;
    // The OS source ID is not publisher attestation. Bound decoded dimensions
    // as well as compressed bytes before handing an untrusted cover to egui.
    let dimensions = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    if !dimensions
        .is_some_and(|(width, height)| width > 0 && height > 0 && width <= 2048 && height <= 2048)
    {
        return Err(PlaybackError::Backend(
            "Spotify artwork format or dimensions are unsupported".into(),
        ));
    }
    Ok(bytes.into())
}

struct Subscription {
    session: Session,
    metadata: i64,
    playback: i64,
    timeline: i64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.metadata);
        let _ = self.session.RemovePlaybackInfoChanged(self.playback);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline);
    }
}

pub struct NativeHost {
    manager: Option<Manager>,
    manager_token: Option<i64>,
    selected: Option<Subscription>,
    dirty: watch::Sender<()>,
    test_controls: bool,
    identity: u64,
    previous_track: Option<LocalTrack>,
    artwork: Option<Arc<[u8]>>,
}

impl NativeHost {
    pub fn new(test_controls: bool, dirty: watch::Sender<()>) -> Result<Self, PlaybackError> {
        // The dedicated thread owns this initialization and its matching Drop.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(api_error)?;
        Ok(Self {
            manager: None,
            manager_token: None,
            selected: None,
            dirty,
            test_controls,
            identity: 0,
            previous_track: None,
            artwork: None,
        })
    }

    fn select(&self) -> Result<Option<Session>, PlaybackError> {
        let Some(manager) = &self.manager else {
            return Ok(None);
        };
        let sessions = manager.GetSessions().map_err(api_error)?;
        let mut candidates = Vec::new();
        for i in 0..sessions.Size().map_err(api_error)? {
            let session = sessions.GetAt(i).map_err(api_error)?;
            let source = session
                .SourceAppUserModelId()
                .map_err(api_error)?
                .to_string();
            candidates.push((source, session));
        }
        select_unique(candidates)
    }

    fn subscribe(&mut self, session: Session) -> Result<(), PlaybackError> {
        self.selected = None;
        let dirty = self.dirty.clone();
        let metadata = session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = dirty.send(());
                Ok(())
            }))
            .map_err(api_error)?;
        let dirty = self.dirty.clone();
        let playback = match session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            let _ = dirty.send(());
            Ok(())
        })) {
            Ok(token) => token,
            Err(error) => {
                let _ = session.RemoveMediaPropertiesChanged(metadata);
                return Err(api_error(error));
            }
        };
        let dirty = self.dirty.clone();
        let timeline =
            match session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = dirty.send(());
                Ok(())
            })) {
                Ok(token) => token,
                Err(error) => {
                    let _ = session.RemoveMediaPropertiesChanged(metadata);
                    let _ = session.RemovePlaybackInfoChanged(playback);
                    return Err(api_error(error));
                }
            };
        self.selected = Some(Subscription {
            session,
            metadata,
            playback,
            timeline,
        });
        self.identity = self.identity.wrapping_add(1);
        self.previous_track = None;
        self.artwork = None;
        Ok(())
    }

    fn capabilities(&self, session: &Session) -> Result<Capabilities, PlaybackError> {
        let info = session.GetPlaybackInfo().map_err(api_error)?;
        let controls = info.Controls().map_err(api_error)?;
        let playing = info.PlaybackStatus().map_err(api_error)? == Status::Playing;
        let timeline = session.GetTimelineProperties().map_err(api_error)?;
        Ok(SessionControls {
            play: controls.IsPlayEnabled().map_err(api_error)?,
            pause: controls.IsPauseEnabled().map_err(api_error)?,
            next: controls.IsNextEnabled().map_err(api_error)?,
            previous: controls.IsPreviousEnabled().map_err(api_error)?,
            seek: controls.IsPlaybackPositionEnabled().map_err(api_error)?
                && timeline.EndTime().map_err(api_error)?.Duration
                    > timeline.StartTime().map_err(api_error)?.Duration,
        }
        .capabilities(playing, self.test_controls))
    }
}

impl SessionHost for NativeHost {
    async fn observe(&mut self) -> Result<Option<SessionSnapshot>, PlaybackError> {
        if self.manager.is_none() {
            let manager = await_operation!(Manager::RequestAsync())?;
            let dirty = self.dirty.clone();
            self.manager_token = Some(
                manager
                    .SessionsChanged(&TypedEventHandler::new(move |_, _| {
                        let _ = dirty.send(());
                        Ok(())
                    }))
                    .map_err(api_error)?,
            );
            self.manager = Some(manager);
        }
        let selection = self.select();
        let session = match selection {
            Ok(Some(session)) => session,
            other => {
                self.selected = None;
                self.previous_track = None;
                self.artwork = None;
                return other.map(|_| None);
            }
        };
        if self
            .selected
            .as_ref()
            .is_none_or(|selected| selected.session != session)
        {
            self.subscribe(session.clone())?;
        }
        let metadata = await_operation!(session.TryGetMediaPropertiesAsync())?;
        let timeline = session.GetTimelineProperties().map_err(api_error)?;
        let start = timeline.StartTime().map_err(api_error)?.Duration;
        let end = timeline.EndTime().map_err(api_error)?.Duration;
        let duration_ms = milliseconds(end.saturating_sub(start));
        let status = session
            .GetPlaybackInfo()
            .map_err(api_error)?
            .PlaybackStatus()
            .map_err(api_error)?;
        let playback = if status == Status::Playing {
            Playback::Playing
        } else if status == Status::Paused {
            Playback::Paused
        } else if status == Status::Changing {
            Playback::Loading
        } else {
            Playback::Stopped
        };
        let title = metadata.Title().map_err(api_error)?.to_string();
        let artist = metadata.Artist().map_err(api_error)?.to_string();
        let mut track = (!title.is_empty()).then(|| LocalTrack {
            // GSMTC supplies no verified Spotify URI. Do not invent one.
            title,
            artists: if artist.is_empty() {
                Vec::new()
            } else {
                vec![ArtistRef {
                    name: artist,
                    ..ArtistRef::default()
                }]
            },
            album: metadata
                .AlbumTitle()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            duration_ms,
            ..LocalTrack::default()
        });
        let mut comparable = self.previous_track.clone();
        if let Some(track) = &mut comparable {
            track.art_url = None;
            track.art_small_url = None;
        }
        if track != comparable {
            // Artwork failure must not discard usable transport/metadata.
            self.artwork = read_artwork(&metadata).await.ok();
        }
        if let (Some(track), Some(bytes)) = (&mut track, &self.artwork) {
            let fingerprint = Sha1::digest(bytes);
            track.art_url = Some(format!("bytes://spotify-session-cover/{fingerprint:x}"));
        }
        self.previous_track = track.clone();
        // An async metadata read may finish after the source disappears or is
        // replaced. Reject it before publishing a stale session snapshot.
        if self.select()?.as_ref() != Some(&session) {
            return Err(PlaybackError::Disconnected);
        }
        let timeline = session.GetTimelineProperties().map_err(api_error)?;
        let anchored = milliseconds(
            timeline
                .Position()
                .map_err(api_error)?
                .Duration
                .saturating_sub(start),
        );
        let position_ms = position_from_anchor(
            anchored,
            duration_ms,
            timeline.LastUpdatedTime().map_err(api_error)?.UniversalTime,
            playback == Playback::Playing,
        );
        Ok(Some(SessionSnapshot {
            identity: self.identity,
            track,
            playback,
            position_ms,
            capabilities: self.capabilities(&session)?,
            artwork: self.artwork.clone(),
        }))
    }

    async fn control(
        &mut self,
        identity: u64,
        command: PlayerCommand,
    ) -> Result<bool, PlaybackError> {
        let session = self.select()?.ok_or(PlaybackError::Disconnected)?;
        if self.identity != identity
            || self
                .selected
                .as_ref()
                .is_none_or(|selected| selected.session != session)
        {
            return Err(PlaybackError::Disconnected);
        }
        if !self.capabilities(&session)?.supports(&command) {
            return Err(PlaybackError::Unsupported(CommandKind::from(&command)));
        }
        match command {
            PlayerCommand::Toggle => await_operation!(session.TryTogglePlayPauseAsync()),
            PlayerCommand::Pause => await_operation!(session.TryPauseAsync()),
            PlayerCommand::Resume => await_operation!(session.TryPlayAsync()),
            PlayerCommand::Next => await_operation!(session.TrySkipNextAsync()),
            PlayerCommand::Previous => await_operation!(session.TrySkipPreviousAsync()),
            PlayerCommand::Seek(position_ms) => {
                let timeline = session.GetTimelineProperties().map_err(api_error)?;
                let start = timeline.StartTime().map_err(api_error)?.Duration;
                let end = timeline.EndTime().map_err(api_error)?.Duration;
                let ticks = start
                    .saturating_add(i64::from(position_ms).saturating_mul(10_000))
                    .clamp(start, end.max(start));
                await_operation!(session.TryChangePlaybackPositionAsync(ticks))
            }
            other => Err(PlaybackError::Unsupported(CommandKind::from(&other))),
        }
    }
}

fn milliseconds(ticks: i64) -> u32 {
    u32::try_from(ticks.max(0) / 10_000).unwrap_or(u32::MAX)
}

fn position_from_anchor(position: u32, duration: u32, updated_ticks: i64, playing: bool) -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let updated_ms = updated_ticks / 10_000;
    let elapsed = if playing && updated_ticks > 0 {
        (now.saturating_add(11_644_473_600_000)
            .saturating_sub(updated_ms.max(0) as u128))
        .min(u128::from(u32::MAX)) as u32
    } else {
        0
    };
    let position = position.saturating_add(elapsed);
    if duration > 0 {
        position.min(duration)
    } else {
        position
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        self.selected = None;
        if let (Some(manager), Some(token)) = (&self.manager, self.manager_token.take()) {
            let _ = manager.RemoveSessionsChanged(token);
        }
        self.manager = None;
        unsafe {
            RoUninitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeline_ticks_are_bounded_and_not_mistaken_for_milliseconds() {
        assert_eq!(milliseconds(12_340_000), 1234);
        assert_eq!(milliseconds(-10), 0);
        assert_eq!(milliseconds(i64::MAX), u32::MAX);
    }
}
