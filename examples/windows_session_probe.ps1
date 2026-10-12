# Isolated Spotify companion experiment. Windows PowerShell 5.1 only.
# No downloads, credentials, Spotify file changes or global media-key dispatch.
[CmdletBinding()]
param(
    [ValidateSet('Play', 'Pause', 'Next', 'Previous')]
    [string[]]$Commands = @(),
    [ValidateRange(0, 300)]
    [int]$WatchSeconds = 0,
    [ValidateRange(0, 300)]
    [int]$MeasureSeconds = 0,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-SpotifySource {
    param([string]$SourceId)
    # Exact known desktop and Store IDs. Never use the system's current session
    # or a substring match, which could select a browser or another media app.
    return $SourceId -ieq 'Spotify.exe' -or
        $SourceId -ieq 'SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify'
}

function Select-SpotifySession {
    param([object[]]$Sessions)
    $candidates = @($Sessions | Where-Object { Test-SpotifySource $_.SourceAppUserModelId })
    if ($candidates.Count -gt 1) {
        throw 'Multiple Spotify sessions. Refusing to choose a command target.'
    }
    if ($candidates.Count -eq 1) { return $candidates[0] }
    return $null
}

function Get-ControlFlag {
    param([string]$Command)
    switch ($Command) {
        'Play' { return 'IsPlayEnabled' }
        'Pause' { return 'IsPauseEnabled' }
        'Next' { return 'IsNextEnabled' }
        'Previous' { return 'IsPreviousEnabled' }
        default { throw 'Unknown command' }
    }
}

if ($SelfTest) {
    $spotify = [pscustomobject]@{ SourceAppUserModelId = 'Spotify.exe' }
    $browser = [pscustomobject]@{ SourceAppUserModelId = 'msedge.exe' }
    $lookalike = [pscustomobject]@{ SourceAppUserModelId = 'NotSpotify.exe' }
    if ($null -ne (Select-SpotifySession @($browser, $lookalike))) {
        throw 'Non-Spotify session selected'
    }
    if ((Select-SpotifySession @($browser, $spotify)).SourceAppUserModelId -ne 'Spotify.exe') {
        throw 'Spotify session not selected'
    }
    if ($null -ne (Select-SpotifySession @())) { throw 'Absent session selected' }
    if (-not (Test-SpotifySource 'SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify')) {
        throw 'Store ID not recognized'
    }
    $rejected = $false
    try { $null = Select-SpotifySession @($spotify, $spotify) } catch { $rejected = $true }
    if (-not $rejected) { throw 'Ambiguous session selected' }
    foreach ($command in @('Play', 'Pause', 'Next', 'Previous')) {
        if ([string]::IsNullOrEmpty((Get-ControlFlag $command))) { throw 'Missing control flag' }
    }
    Write-Output 'PASS: absent, exact desktop/Store identity, unrelated apps, ambiguity and control mapping'
    return
}

if ($PSVersionTable.PSEdition -ne 'Desktop') {
    throw 'Run with Windows PowerShell 5.1 (powershell.exe), not PowerShell 7.'
}

Add-Type -AssemblyName System.Runtime.WindowsRuntime
$managerType = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager, Windows.Media.Control, ContentType = WindowsRuntime]
$propertiesType = [Windows.Media.Control.GlobalSystemMediaTransportControlsSessionMediaProperties, Windows.Media.Control, ContentType = WindowsRuntime]
$script:asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() |
    Where-Object {
        $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and
        $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
    } | Select-Object -First 1

function Wait-WinRT {
    param($Operation, [type]$ResultType)
    $task = $script:asTask.MakeGenericMethod($ResultType).Invoke($null, @($Operation))
    if (-not $task.Wait(5000)) {
        $Operation.Cancel()
        throw 'WinRT operation timed out after five seconds'
    }
    return $task.GetAwaiter().GetResult()
}

function Read-SpotifyState {
    param($Session)
    $info = $Session.GetPlaybackInfo()
    $timeline = $Session.GetTimelineProperties()
    $metadata = Wait-WinRT $Session.TryGetMediaPropertiesAsync() $propertiesType
    return [pscustomobject]@{
        SourceId = $Session.SourceAppUserModelId
        Playback = $info.PlaybackStatus.ToString()
        Title = $metadata.Title
        Artist = $metadata.Artist
        Album = $metadata.AlbumTitle
        PositionMs = $timeline.Position.TotalMilliseconds
        StartMs = $timeline.StartTime.TotalMilliseconds
        EndMs = $timeline.EndTime.TotalMilliseconds
        Advertised = [pscustomobject]@{
            Play = $info.Controls.IsPlayEnabled
            Pause = $info.Controls.IsPauseEnabled
            Next = $info.Controls.IsNextEnabled
            Previous = $info.Controls.IsPreviousEnabled
            Seek = $info.Controls.IsPlaybackPositionEnabled
        }
    }
}

function Get-PlaybackProcesses {
    # All Spotify processes, not only its main window. Values are diagnostics,
    # not a benchmark of a yet-to-be-implemented Rust companion adapter.
    return @(Get-Process | Where-Object { $_.ProcessName -in @('Spotify', 'spotifast') })
}

function Write-MemorySample {
    $processes = @(Get-PlaybackProcesses)
    $hasSpotify = @($processes | Where-Object ProcessName -eq 'Spotify').Count -gt 0
    $hasSpotifast = @($processes | Where-Object ProcessName -eq 'spotifast').Count -gt 0
    $workingSet = 0L
    $privateBytes = 0L
    foreach ($process in $processes) {
        $workingSet += $process.WorkingSet64
        $privateBytes += $process.PrivateMemorySize64
    }
    [pscustomobject]@{
        Kind = 'process_memory'
        CombinedPlaybackMeasurement = $hasSpotify -and $hasSpotifast
        Processes = @($processes | ForEach-Object {
            [pscustomobject]@{
                Name = $_.ProcessName
                Pid = $_.Id
                WorkingSetBytes = $_.WorkingSet64
                PrivateBytes = $_.PrivateMemorySize64
            }
        })
        WorkingSetBytes = if ($hasSpotify -and $hasSpotifast) { $workingSet } else { $null }
        PrivateBytes = if ($hasSpotify -and $hasSpotifast) { $privateBytes } else { $null }
        Note = 'Working-set sums may double-count shared pages. Missing processes do not represent zero-cost playback.'
    } | ConvertTo-Json -Depth 5
}

$manager = Wait-WinRT $managerType::RequestAsync() $managerType
$sessions = @($manager.GetSessions())
[pscustomobject]@{
    Kind = 'sessions'
    SourceIds = @($sessions | ForEach-Object { $_.SourceAppUserModelId })
    Count = $sessions.Count
} | ConvertTo-Json

try {
    $selected = Select-SpotifySession $sessions
    if ($null -eq $selected) {
        [pscustomobject]@{ Kind = 'state'; Status = 'Spotify session unavailable' } | ConvertTo-Json
    } else {
        Read-SpotifyState $selected | ConvertTo-Json -Depth 4
    }
} catch {
    [pscustomobject]@{ Kind = 'state'; Status = 'Unavailable'; Error = $_.Exception.Message } | ConvertTo-Json
}

foreach ($command in $Commands) {
    # Re-enumerate every time. A disappearing Spotify session never causes a
    # fallback to another application's session.
    try {
        $selected = Select-SpotifySession @($manager.GetSessions())
        if ($null -eq $selected) { throw 'Spotify session unavailable or disappeared' }
        $before = Read-SpotifyState $selected
        $flag = Get-ControlFlag $command
        if (-not $selected.GetPlaybackInfo().Controls.$flag) {
            [pscustomobject]@{ Command = $command; Advertised = $false; Attempted = $false } | ConvertTo-Json
            continue
        }
        $operation = switch ($command) {
            'Play' { $selected.TryPlayAsync() }
            'Pause' { $selected.TryPauseAsync() }
            'Next' { $selected.TrySkipNextAsync() }
            'Previous' { $selected.TrySkipPreviousAsync() }
        }
        $accepted = Wait-WinRT $operation ([bool])
        Start-Sleep -Milliseconds 500
        $afterSession = Select-SpotifySession @($manager.GetSessions())
        $after = if ($null -ne $afterSession) { Read-SpotifyState $afterSession } else { $null }
        [pscustomobject]@{
            Command = $command
            Advertised = $true
            Attempted = $true
            Accepted = $accepted
            Before = $before
            After = $after
            Note = 'Acceptance and a metadata snapshot do not prove audible playback. Observe the sound separately.'
        } | ConvertTo-Json -Depth 6
    } catch {
        [pscustomobject]@{ Command = $command; Status = 'Failed or unavailable'; Error = $_.Exception.Message } | ConvertTo-Json
    }
}

$watch = [System.Diagnostics.Stopwatch]::StartNew()
while ($watch.Elapsed.TotalSeconds -lt $WatchSeconds) {
    Start-Sleep -Seconds 2
    try {
        $selected = Select-SpotifySession @($manager.GetSessions())
        if ($null -eq $selected) {
            [pscustomobject]@{ Kind = 'state'; Status = 'Spotify session unavailable or disappeared' } | ConvertTo-Json
        } else {
            Read-SpotifyState $selected | ConvertTo-Json -Depth 4
        }
    } catch {
        [pscustomobject]@{ Kind = 'state'; Status = 'Unavailable'; Error = $_.Exception.Message } | ConvertTo-Json
    }
}

Write-MemorySample
if ($MeasureSeconds -gt 0) {
    $beforeCpu = @{}
    $beforeProcesses = @(Get-PlaybackProcesses)
    foreach ($process in $beforeProcesses) {
        $beforeCpu[$process.Id] = $process.TotalProcessorTime.TotalSeconds
    }
    $interval = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $MeasureSeconds
    $afterProcesses = @(Get-PlaybackProcesses)
    $cpuSeconds = 0.0
    foreach ($process in $afterProcesses) {
        if ($beforeCpu.ContainsKey($process.Id)) {
            $cpuSeconds += $process.TotalProcessorTime.TotalSeconds - $beforeCpu[$process.Id]
        }
    }
    $stable = $afterProcesses.Count -eq $beforeCpu.Count -and
        @($afterProcesses | Where-Object { -not $beforeCpu.ContainsKey($_.Id) }).Count -eq 0
    $combined = $stable -and
        @($afterProcesses | Where-Object ProcessName -eq 'Spotify').Count -gt 0 -and
        @($afterProcesses | Where-Object ProcessName -eq 'spotifast').Count -gt 0
    [pscustomobject]@{
        Kind = 'process_cpu'
        CombinedPlaybackMeasurement = $combined
        Seconds = $interval.Elapsed.TotalSeconds
        CorePercent = if ($combined) { 100 * $cpuSeconds / $interval.Elapsed.TotalSeconds } else { $null }
        MachinePercent = if ($combined) { 100 * $cpuSeconds / $interval.Elapsed.TotalSeconds / [Environment]::ProcessorCount } else { $null }
        ProcessSetStable = $stable
        Note = 'Changed process sets invalidate the sample; only surviving PIDs contribute.'
    } | ConvertTo-Json
    Write-MemorySample
}
