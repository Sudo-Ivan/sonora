<div align="center">

<img src="./.github/readme-icon.svg" alt="" width="60" height="60">

<h1>Sonora</h1>

<p align="center">
  <a href="https://sonorahq.org/changelog">Changelog</a>
  ·
  <a href="https://sonorahq.org/docs">Docs</a>
  ·
  <a href="https://sonorahq.org/docs/roadmap">Roadmap</a>
  ·
  <a href="https://sonorahq.org/faq">FAQ</a>
</p>

[![Build](https://img.shields.io/github/actions/workflow/status/Sudo-Ivan/sonora/ci.yml?style=flat-square&label=build)](https://github.com/Sudo-Ivan/sonora/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/Sudo-Ivan/sonora?style=flat-square&label=license)](./COPYING)

### A native music streaming client, built with Rust and GPUI

Stream from your favorite services and play local files - all in one **native** app.

A fork of [sonorahq/sonora](https://github.com/sonorahq/sonora); see [FORK.md](FORK.md) for what differs.
</div>

<div align="center">
    <table>
      <tr>
        <td colspan="2">
          <img width="1613" height="981" alt="image" src="https://github.com/user-attachments/assets/7952a912-7fbc-4186-b467-a08dd7e71e22" />
        </td>
      </tr>
      <tr>
        <td width="50%">
          <img width="1623" height="987" alt="image" src="https://github.com/user-attachments/assets/580bf9d6-db85-4fde-b599-82ba2a28cc51" />
        </td>
        <td width="50%">
          <img width="1623" height="987" alt="image" src="https://github.com/user-attachments/assets/64fcd709-5917-432c-a418-2e07527343d2" />
        </td>
      </tr>
    </table>
</div>
<div align="center">
    <sub>
      Adaptive themes are optional. Everything is (or will be) customizable.
    </sub>
</div>

> [!IMPORTANT]
> **Sonora is not a piracy tool.**
>
> Sonora is not a platform for obtaining or sharing copyrighted material. We will not implement any functions that can be used to export decrypted streams, DRM licenses, content keys, or to convert protected streams into media files.
>
> Sonora is not designed to circumvent subscriptions or other restrictions put in place by music streaming platforms. If the service demands that you have a valid subscription in order to play back their tracks, so will Sonora.
>
> Features aimed at ripping, downloading, distributing, or gaining access to protected streaming content are out of scope for the project.

## Features

* **Deezer, Tidal, Subsonic/Navidrome** and local playback
* Gapless playback, audio normalization, shuffle, sleep timer
* Synced/karaoke lyrics, background vocals, and romanization
* Scrobbling with LastFM, ListenBrainz, LibreFM, and Maloja
* Self-hosted servers work over HTTPS with self-signed certificates
* Themes, fonts, icons, transparency, blur, and window styling

## Installation

### macOS

Download the disk image from the
[latest release](https://github.com/Sudo-Ivan/sonora/releases/latest), then:

```sh
xattr -dr com.apple.quarantine /Applications/Sonora.app
```

### Linux

#### Arch

Install from the AUR with your AUR helper of choice:

```sh
yay -S sonora-bin
```

`sonora-bin` installs the prebuilt release binary. `sonora` builds the same version from source
instead, which takes a while on a Rust and GPUI tree but links against your own system libraries:

```sh
yay -S sonora
```

Either `pipewire-alsa` or `pulseaudio-alsa` is required, matching your sound server. Sonora
warns you at startup when the one your sound server needs is missing.

#### Flatpak

Add the Sonora repository (updates with `flatpak update`):

```sh
flatpak install --user https://sudo-ivan.github.io/sonora/sonora.flatpakref
```

#### AppImage

Download the `x86_64` AppImage from the
[latest release](https://github.com/Sudo-Ivan/sonora/releases/latest), make it executable and run
it:

```sh
chmod +x sonora-*.AppImage
./sonora-*.AppImage
```

An `aarch64` build is published beside it. The AppImage carries no Vulkan driver and no ALSA
bridge, so both still come from your system. It does not update itself, but it carries its update
information, so [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) or an
AppImage manager such as [AppManager](https://github.com/kem-a/AppManager) can fetch a new release
for you.

### Nix

```nix
inputs.sonora.url = "github:Sudo-Ivan/sonora";
```

```text
inputs.sonora.packages.${system}.default     (prebuilt where available, else from source)
inputs.sonora.packages.${system}.sonora      (same as default)
inputs.sonora.packages.${system}.sonora-bin  (prebuilt, x86_64-linux, aarch64-linux, aarch64-darwin)
inputs.sonora.packages.${system}.sonora-src  (always built from source)
```

The prebuilt package downloads the latest tagged release. `sonora-src` compiles whatever
revision of the flake you point it at, so pin the input to a tag such as
`github:sonorahq/sonora/v0.43.0` to build a release. Building from source on macOS needs Xcode
added to the Nix store, because the shaders compile with Apple's Metal compiler.

You can set configuration options via the included Home Manager module under `programs.sonora`:

```nix
{
  imports = [ inputs.sonora.homeManagerModules.default ];
  programs.sonora = {
    enable = true;
    settings = {
      provider = "deezer";
      appearance.theme = "dark";
    };
  };
}
```

### Windows

#### Installer

Download and run the [installer](https://github.com/Sudo-Ivan/sonora/releases/latest/download/Sonora-Setup.exe),
or the [ARM installer](https://github.com/Sudo-Ivan/sonora/releases/latest/download/Sonora-Setup-arm64.exe)
on Windows on ARM.

#### Portable

Download the latest `windows-msvc.exe` for your architecture from [Releases](https://github.com/Sudo-Ivan/sonora/releases/latest).

## Provenance

Every release is built by GitHub Actions from the tagged commit, and each binary carries an
[artifact attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations)
you can verify:

```sh
gh attestation verify sonora-v0.42.0-x86_64-unknown-linux-gnu --repo Sudo-Ivan/sonora
```

Every release also carries an SLSA v1 provenance bundle, generated by the
[slsa-github-generator](https://github.com/slsa-framework/slsa-github-generator) reusable
workflow, which runs isolated from the build jobs so a compromised build cannot forge it.
Verify offline with [slsa-verifier](https://github.com/slsa-framework/slsa-verifier):

```sh
slsa-verifier verify-artifact sonora-v0.42.0-x86_64-unknown-linux-gnu \
  --provenance-path sonora-v0.42.0.intoto.jsonl \
  --source-uri github.com/Sudo-Ivan/sonora --source-tag v0.42.0
```

Releases are published once and never modified afterwards.

## Translations

<!-- i18n:start -->

| Language | Translated | Coverage |
| --- | --- | --- |
| English (`en-US`) | 1067/1067 | 100% |
| Deutsch (`de`) | 1011/1067 | 95% |
| Español (`es`) | 1011/1067 | 95% |
| Français (`fr`) | 1011/1067 | 95% |
| Italiano (`it`) | 1011/1067 | 95% |
| Bahasa Indonesia (`id`) | 1011/1067 | 95% |
| 日本語 (`ja`) | 1011/1067 | 95% |
| Русский (`ru`) | 1011/1067 | 95% |
| Українська (`uk`) | 1011/1067 | 95% |
| Polski (`pl`) | 1011/1067 | 95% |
| Čeština (`cs`) | 1012/1067 | 95% |
| Português (Brasil) (`pt-BR`) | 1011/1067 | 95% |
| 简体中文 (`zh-CN`) | 1011/1067 | 95% |
| Türkçe (`tr`) | 1011/1067 | 95% |
| Shqip (`sq`) | 1012/1067 | 95% |

<!-- i18n:end -->

## Credits

Sonora is built with the help of some incredible open-source projects, including:

- [Zed](https://github.com/zed-industries/zed) - a wonderful editor (~~ab~~)used by all core team members. Conveniently provides `gpui` - their native Rust rendering stack.

## License

Copyright (C) 2026 Sonora Contributors.

Sonora is free software, released under the [GNU General Public License version
3 or later](COPYING).

The binary also embeds the [Inter](https://github.com/rsms/inter) typeface (SIL
Open Font License 1.1) and four interchangeable icon sets:
[Lucide](https://lucide.dev) (ISC), [Iconoir](https://iconoir.com) (MIT),
[Remix Icon](https://remixicon.com) 4.8.0 (Apache 2.0) and the
[Solar](https://www.figma.com/community/file/1166831539721848736) Linear set
(CC BY 4.0, by 480 Design). Each pack keeps its licence beside its files in
`assets/icons`. `THIRD-PARTY.md` lists every bundled dependency.
