# Fork notes

This repository is a fork of [sonorahq/sonora](https://github.com/sonorahq/sonora),
maintained at [Sudo-Ivan/sonora](https://github.com/Sudo-Ivan/sonora). Everything
not listed below is inherited from upstream.

## Identity

- Repository metadata, the update checker and the HTTP user-agent strings point at
  `Sudo-Ivan/sonora`.
- The Flatpak application id is `io.github.sudo_ivan.sonora`; the metainfo file is
  `flatpak/io.github.sudo_ivan.sonora.metainfo.xml`. The macOS bundle identifier
  matches it.
- The Flatpak repository is published to `sudo-ivan.github.io/sonora`, served
  by GitHub Pages from the `flatpak-repo` branch. `GPGKey` in
  `flatpak/pages/sonora.flatpakref` and `flatpak/pages/sonora.flatpakrepo`
  carries the fork's signing key, whose private half lives in the
  `FLATPAK_GPG_KEY` secret.
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

- `.github/workflows/ci.yml` is the gate: rustfmt, clippy -D warnings and the
  full workspace test suite on push and pull request, clippy on the macOS and
  Windows targets on pull requests, and a release build of every target on
  pushes to `master`. Each built artefact runs `sonora --version` as a smoke
  test before it is staged.
- Benchmarks gate the merge too: the ignored bench tests print machine-readable
  BENCH lines, and `scripts/bench-check.py` fails the `bench` job when a metric
  lands over its ceiling in `bench/baselines.json`. Regenerate ceilings with
  `--write` when a perf change is intentional.
- The `ci` job is the one name to require in branch protection: it needs every
  other job and fails unless each succeeded or was skipped, so adding a job
  later cannot silently leave a hole in the rule. Every job carries a
  `timeout-minutes` bound.
- `.github/workflows/release.yml` builds all six release targets, the universal
  macOS disk image and both AppImages, attaches Sigstore build provenance to
  every artefact, and publishes a `v*` tag as a GitHub release through
  `gh release`: the release is created as a draft with all assets on it, then
  published once. Enable "immutable releases" and tag protection in the
  repository settings so a published release and its tag cannot be mutated.
- SLSA provenance comes from the pinned `slsa-github-generator` generic builder,
  a reusable workflow that signs in isolation from the build jobs (SLSA level 3
  style isolation) and attaches `sonora-<tag>.intoto.jsonl` to the draft before
  it publishes. Users verify with `gh attestation verify` or `slsa-verifier`
  offline, as documented under "Provenance" in the README.
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

The library fork under `github.com/sonorahq` (`gpui`) stays pinned
dependencies: upstream maintains them for this code base and they carry changes its
own projects depend on. The `librespot` fork is gone with the Spotify provider.
