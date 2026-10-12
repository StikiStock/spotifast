# Windows Spotify companion (experimental)

In this independent fork, **Settings → Playback on this computer → Playback
backend → Windows Spotify Session (experimental)** attaches to the already
running Spotify desktop application's Windows media session. Librespot remains
the default. Switching back restores its normal authentication and audio path.

Start Spotify and play a song there. The existing Spotifast player bar then
shows its title, artist, duration, position and play/pause state. The album is
included in the existing title's hover tooltip without adding a player-bar row.
URI-dependent liking, lyrics and queue saving are unavailable for the external
item; library liking still works normally. Metadata
and playback changes arrive through native Windows notifications. A ten-second
background recovery check handles missed notifications and restarts. Position
advances locally between updates using the session's timeline anchor, including
Windows' last-update timestamp. Missing duration is treated as unknown.

This is a control adapter for Spotify's audio engine. It does not provide
standalone streaming, remove advertisements, change account entitlements,
install or patch Spotify, run PowerShell, or supply Spotify audio to Spotifast's
EQ and visualizers.

## Controls and limitations

- **Play/pause and Next:** wired to native asynchronous session commands, gated
  by the source's current availability flags. Your successful PowerShell probe
  verified those controls on your Spotify installation. The new native Rust/UI
  path still needs live validation.
- **Previous and Seek:** disabled by default. Enable **Test Previous and Seek**
  in the same settings section only when performing the live checks below.
  This opt-in is not a claim of verified support, and an OS-disabled control
  remains disabled. Seek converts milliseconds to Windows' 100-nanosecond ticks,
  accounts for the timeline start, and clamps to its advertised end.
- **Track, album and playlist selection:** choose the actual playback in
  Spotify. Spotifast rejects these playback actions before changing its track
  or queue and explains why. GSMTC supplies no verified Spotify URI loader or
  queue API. A title is never turned into a guessed Spotify URI.
- **Queue, volume, shuffle and repeat:** control these in Spotify. The companion
  cannot edit/read the external queue or set its volume. These operations cannot
  fall back to the Premium-only Web API. The native player bar disables
  unsupported controls; keyboard and mini-player requests are also guarded.
- **Artwork:** reads Spotify's optional Windows thumbnail locally, with timeouts
  and an 8 MiB byte limit, with decoded dimensions limited to 2048 by 2048.
  Missing/rejected artwork leaves usable metadata
  intact. Only the current cover is retained by the companion. Cover support
  needs live testing on desktop and Store versions.

Library and search retain the existing Web API sign-in. The companion itself
needs no playback grant or Premium check. A saved companion selection prevents
librespot audio startup and playback authorization, including for a Free
account. Browsing features which depend exclusively on a librespot session,
such as live folder reads or radio resolution, do not become available from
Windows media-session metadata.

The original local playback resume point and queue are not replaced with an
unknown external URI. Companion playback is not written as fabricated Spotify
history. Spotifast also stops publishing its own system media session in this
mode, leaving Windows media keys with Spotify instead of dispatching them twice.

## Connection safety and shutdown

Only exact known source IDs are accepted: `Spotify.exe` and the Spotify Store
application ID. No substring match or fallback to the system's current session
is used. Multiple candidates disable attachment and explain that the extra
instance must be closed. Source IDs are identification, not publisher attestation.

Every command re-enumerates the sessions and checks the same selected object and
its current capabilities. Late callbacks from a prior backend selection are
discarded by generation. Unexpected session loss clears unavailable controls;
restart notifications and the recovery timer attach to the new session. A
rejected command retains the current metadata instead of flashing an empty row.

All Windows Runtime work runs on a dedicated MTA thread, using a current-thread
Tokio runtime for bounded asynchronous calls. The UI sends bounded channel
commands and reads cached state. Disconnect signals the worker, which removes
event subscriptions, releases its Windows objects and uninitializes WinRT.
Spotify's audio remains running independently when Spotifast closes.

## Live test checklist

Use the [GitHub Actions build](windows-remote-build.md). No Visual Studio Build
Tools are required on your PC to run the downloaded executable.

1. Start Spotify, sign in to Spotifast for browsing, and choose Windows Session.
   Check the metadata and duration. Pause, resume, and press Next in Spotifast.
   Confirm actual sound and the matching transition in Spotify.
2. Pause/resume and change songs inside Spotify. Check automatic updates and
   position interpolation. Confirm no backward jump at each ten-second recovery
   check and no advancing position while paused.
3. Enable **Test Previous and Seek**. Test Previous both near the beginning and
   later in a song. Drag Seek forward/backward and to the ends. Confirm audible
   position and the Spotify timeline. Report advertised disabled controls rather
   than assuming all content permits them. Disable the opt-in after testing if
   either command fails.
4. Close and restart Spotify. Check waiting state, no stale metadata, automatic
   reattachment, and controls after restart. Run another media application and
   verify that only Spotify is controlled. Test multiple Spotify sessions where
   available; attachment must stop rather than choose an arbitrary instance.
5. Repeat with your Free account and during restricted/advertisement states.
   Account restrictions and advertisements remain Spotify's responsibility.
   Check browsing without a local playback grant and verify no librespot audio
   starts in companion mode.
6. Check artwork changes, light/dark themes and narrow/normal window sizes.
   Confirm selecting a track in Spotifast produces an explanation without
   changing the shown external track or its queue. Switch to Librespot and
   verify the existing Premium playback and queue recovery still work.

No combined RAM/CPU benchmark of this native adapter has been obtained. The
optional PowerShell probe remains available for diagnostics and process
sampling, but normal companion use does not need it.

## Current validation

The earlier Actions success and live PowerShell milestones were reported by
the user. They validate the previous build and probe, not this unpushed native
implementation. New native-driver/MSVC-target checks and mock tests are recorded
in the feasibility report. The workflow also runs the existing deterministic
`demo` UI tests without MilkDrop, then builds and uploads the same executable
artifact after all gates pass. No commits or pushes have been made by the agent.
