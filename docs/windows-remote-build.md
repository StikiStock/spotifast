# Build the experimental Windows executable on GitHub

The fork's [Windows experimental workflow](../.github/workflows/windows-build.yml)
uses a GitHub-hosted `windows-2022` machine, its preinstalled Visual C++ tools
and Windows SDK, and the project's Rust 1.98.0 toolchain. Your computer needs
only Git to upload the source and a browser to download the result.

Every Cargo check, test, lint and release build uses `--no-default-features`.
The test suite also runs with `--features demo` for deterministic UI coverage.
MilkDrop is excluded. Formatting checks the whole workspace. The job does not
run packaging scripts, create an installer, build Jekyll, or start Spotify.

## Review, commit and push when ready

These commands are instructions for you. The agent has not committed or pushed.
Replace `YOUR_GITHUB_USERNAME` with the owner of your existing GitHub fork.
Create the fork through GitHub first if you do not have one yet.
Open its **Actions** tab and enable workflows before pushing, if they are
disabled for the fork. Enabling them later does not replay an earlier push;
use the manual-run setup below in that case.

The current `origin` points to upstream `crmne/spotifast`. Add a separate `fork`
remote so the upstream address is preserved and the push goes to your account.
If `fork` already exists, inspect `git remote -v` and use its existing correct
address rather than adding it again.

From PowerShell:

```powershell
Set-Location C:\my-workspace\spotifast
git branch --show-current
git status --short
git diff
Get-Content .github/workflows/windows-build.yml

git remote add fork https://github.com/YOUR_GITHUB_USERNAME/spotifast.git
git remote -v
```

Confirm the branch is `experimental-playback` and the `fork` push URL is your
repository. Review the untracked `src/playback/`, probe and report as well.
The user reports that the previous native Windows executable compiled
successfully. The companion-only sign-in changes need another run. After
reviewing these changes, commit them together because the settings, worker and
UI entry path depend on each other:

```powershell
git add README.md docs/_reference/how-it-connects.md docs/windows-companion.md docs/windows-remote-build.md docs/playback-feasibility.md src/app.rs src/backend.rs src/model.rs src/player.rs src/settings.rs src/playback/session.rs src/ui/login.rs src/ui/mod.rs src/ui/settings.rs src/ui/sidebar.rs src/ui/topbar.rs
git diff --cached
git commit -m "Allow Windows companion use without Spotifast sign-in"
git push fork experimental-playback
```

The checkout already has `fork` pointing to `StikiStock/spotifast`; `origin`
continues to point to upstream. The push triggers the Windows workflow. These
new application/authentication tests are not validated until that run succeeds.
The local `target/` and `.cache/` directories remain ignored and are not staged.
No agent commit or push has been made.

## Run and download

In your fork, open **Actions**. With workflows enabled, the push starts
**Windows experimental build** automatically.
Select the run for `experimental-playback`, then inspect Formatting, Check,
Test, Clippy and Build. Any failure prevents the artifact upload; the failed
step's log contains the compiler/test diagnostic to fix next.

For manual runs, GitHub
[requires the workflow file on the default branch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
The simplest setup for this independent experimental fork is to set its default
branch to `experimental-playback` in the repository's **Settings → Default
branch** after pushing. Alternatively, keep the existing default branch and
add this workflow there separately. Merely adding `workflow_dispatch` on a
non-default branch does not guarantee the manual button will appear. Do not
merge the playback changes into main just to obtain a manual trigger.

Once registered, select **Actions → Windows experimental build → Run workflow**,
choose `experimental-playback`, and run it. The job intentionally skips any
other selected branch.

After the run succeeds, open its summary, find **Artifacts**, and download
`spotifast-windows-x64-no-milkdrop`. Extract the ZIP and run `spotifast.exe`.
The artifact is retained for 14 days, subject to repository retention limits.
You need to be signed in with repository read access to
[download an artifact](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/download-workflow-artifacts).
There is no installer and MilkDrop is unavailable in this build.
Start Spotify, then select the native companion in Spotifast settings using
the [companion guide and live-test checklist](windows-companion.md).
The PowerShell probe below is now optional diagnostic tooling.

If you use the GitHub CLI, replace `YOUR_GITHUB_USERNAME/spotifast` and `RUN_ID`:

```powershell
gh run list --repo YOUR_GITHUB_USERNAME/spotifast --workflow windows-build.yml --branch experimental-playback
gh run watch RUN_ID --repo YOUR_GITHUB_USERNAME/spotifast --exit-status
gh run download RUN_ID --repo YOUR_GITHUB_USERNAME/spotifast --name spotifast-windows-x64-no-milkdrop --dir dist/windows-experimental
```

After the workflow is registered on the default branch, manually trigger it:

```powershell
gh workflow run windows-build.yml --repo YOUR_GITHUB_USERNAME/spotifast --ref experimental-playback
```

## Run the session probe without Rust

Open the installed Spotify desktop app, sign in there and play a song. Then
open **Windows PowerShell 5.1** (`powershell.exe`, not PowerShell 7) and run:

```powershell
Set-Location C:\my-workspace\spotifast
./examples/windows_session_probe.ps1 -SelfTest
./examples/windows_session_probe.ps1 -WatchSeconds 10
```

The second command observes sessions, metadata, playback state and advertised
control flags. It does not issue playback commands. If the script is blocked
by your PowerShell execution policy, invoke it with a policy override confined
to that new process, without changing your machine's policy:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\examples\windows_session_probe.ps1 -WatchSeconds 10
```

From inside Windows PowerShell 5.1, explicitly test controls:

```powershell
./examples/windows_session_probe.ps1 -Commands Pause,Play -WatchSeconds 5
./examples/windows_session_probe.ps1 -Commands Next,Previous -WatchSeconds 5
```

These commands actually pause/resume or skip Spotify. The probe checks
advertised capability flags, reselects Spotify before each command, and reports
the `Try*` acceptance result and before/after snapshots. Listen separately:
acceptance does not guarantee audible playback. Missing or ambiguous Spotify
sessions produce an unavailable/error report; the probe never falls back to
another media app. It neither compiles Rust nor installs or patches Spotify.

After downloading a working executable, optional combined process sampling
requires both Spotifast and Spotify to be running:

```powershell
./examples/windows_session_probe.ps1 -MeasureSeconds 60
```

Metadata output contains song titles and artists. Review it before sharing.
No OAuth tokens, passwords or API keys are needed for the probe.

## Safety and validation status

The workflow has only `contents: read`, disables persisted checkout credentials,
pins its three actions to reviewed release commits, and passes no Spotify or
repository secrets to Cargo. Ordinary tests use fake grants; ignored account
and native-store tests are not enabled. Only the exact release `.exe` path is
uploaded after all prior steps succeed. No state directories, logs, caches,
debug symbols or entire workspace are uploaded, and no release is published.

The local review checks workflow syntax and invariants, Rust formatting, and
probe self-tests. No new remote run can be made until the source is committed
and pushed with your approval. A successful build validates compilation/tests,
not a Spotify account session, Free playback or advertisement removal.
