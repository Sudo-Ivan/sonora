# Fork notes

This repository is a fork of [sonorahq/sonora](https://github.com/sonorahq/sonora),
maintained at [Sudo-Ivan/sonora](https://github.com/Sudo-Ivan/sonora). Everything
not listed below is inherited from upstream.

## Identity

- Repository metadata, the update checker and the HTTP user-agent strings point at
  `Sudo-Ivan/sonora`.
- The Flatpak application id is `io.github.sudo-ivan.sonora`; the metainfo file is
  `flatpak/io.github.sudo-ivan.sonora.metainfo.xml`. The macOS bundle identifier
  matches it.
- The Flatpak repository is published to `sudo-ivan.github.io/sonora`. The
  `GPGKey` lines in `flatpak/pages/sonora.flatpakref` and
  `flatpak/pages/sonora.flatpakrepo` are intentionally empty: they need this
  fork's own signing key, as does the `FLATPAK_GPG_KEY` secret below.
- Upstream's Discord and Matrix links are gone, as is the team roster in
  Settings.

## Features

- The Spotify and Apple Music providers are removed: no librespot dependency, no
  Widevine CDM handling, and no calls to either service. The `spotify:` deep link
  scheme and the account, lyrics and settings entries that named either provider are
  gone with them.
- Discord Rich Presence and its settings page section are removed, along with the
  `discord-rich-presence` dependency, the Deezer cover lookup, and the per-track
  publicity plumbing (`public_art`, `set_artist_art`, `Playing::can_public`) that
  existed only to feed it.
- The one-time anonymous installation report is removed; the app sends no
  telemetry.
- Client-side mixes: "Start mix" on a track builds a run from the library with a
  local scorer (shared artists, album, genre tags, era, duration, popularity)
  with artist and album spacing. No service is asked for recommendations.
- Forever radio: a queue-header toggle keeps refilling the queue from the whole
  library, spread across artists and skipping recently played tracks. Provider
  radio keeps precedence where the service answers; the local picks are the
  floor.
- Subsonic gained OpenSubsonic detail mapping (genres into tags, release dates,
  playlist change dates, starred timestamps) and uses the sonicSimilarity
  extension for radio when the server offers it.
- Local libraries import .m3u and .m3u8 playlist files found during the folder
  scan. Entries resolve against the playlist's folder, as absolute paths, as
  file:// uris, or with Windows separators, and land as ordinary local
  playlists, rebuilt on each scan that sees the file again.
- Middle-click paste into text fields uses the X/Wayland primary selection.
- Fullscreen no longer paints a sibling's cached cover over a track with no
  art, and hand-queued tracks no longer double in the queue when shuffle is
  toggled after a restart.
- The queue keeps its tracks shared rather than duplicated between the source
  and the play order, and the memory watcher squeezes the artwork cache early
  when resident memory runs high.
- Self-hosted Subsonic and Maloja servers can be reached over HTTPS with a
  self-signed or otherwise invalid certificate. The sign-in and scrobble dialogs
  offer "Trust a self-signed or otherwise invalid certificate". The choice is
  stored with the account, applies to the API client, media streams and cover
  art, and is scoped to that server's authority: every other connection keeps
  full verification. Forgetting the account drops the exception.
- On Linux the process sandboxes itself with Landlock. It can write only its
  own config, data, cache and state directories plus the configured local
  music folders, and read only the system directories it loads or resolves
  from. Inside Flatpak, which sandboxes already, nothing is installed, and
  `SONORA_SANDBOX=0` disables it.

## Continuous integration

- `.github/workflows/ci.yml` runs rustfmt, clippy and the test suite on push and
  pull request, lints the macOS and Windows cfgs, and builds every release
  target on `main` pushes.
- `.github/workflows/release.yml` builds all six release targets, the universal
  macOS disk image and both AppImages, attaches Sigstore build provenance to
  every artefact, and publishes a `v*` tag as a GitHub release through
  `gh release`: the release is created as a draft with all assets on it, then
  published once. Enable "immutable releases" and tag protection in the
  repository settings so a published release and its tag cannot be mutated.
- Every third-party action is pinned to a full commit SHA, every job declares
  least-privilege permissions, no caches run on tag builds, and `zizmor` audits
  the workflows on every run. `Quad4-Software/argus` scans the dependency tree
  and OSV advisories, installed pinned by commit because its composite action
  resolves the tool unpinned. Accepted findings live in `.argusignore`.
  Dependabot keeps the pinned SHAs moving behind a cooldown.
- The upstream automation that depended on `TRIAGE_API_KEY`, the Sonora Buddy bot
  account and Discord webhooks is deleted: `announce`, `triage`, `stale`,
  `pr-labels` and `flatpak-bundles` workflows plus `.github/triage/`.
- The Flatpak repository jobs run only when the `FLATPAK_REPO` repository
  variable is `true`; they need a `FLATPAK_GPG_KEY` secret holding an ASCII
  armoured private key, the matching `GPGKey` lines in `flatpak/pages`, and
  GitHub Pages served from the `flatpak-repo` branch.

## Still upstream

The library forks under `github.com/sonorahq` (`gpui`, `ytmusic-rs`) stay pinned
dependencies: upstream maintains them for this code base and they carry changes its
own projects depend on. The `librespot` fork is gone with the Spotify provider.
