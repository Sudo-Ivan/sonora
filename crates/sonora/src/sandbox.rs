//! Linux filesystem sandbox
//!
//! `apply` installs a Landlock domain that leaves the process able to write only its own XDG
//! directories and the folders the user added as local libraries, while the system directories
//! it loads, resolves and opens from stay readable. It runs before the instance listener and the
//! tokio runtime exist, because a thread created after `restrict_self` inherits the domain and
//! one created before it never does. The sandbox is defense in depth, not a gate: any failure or
//! an old kernel is logged and the process simply runs unsandboxed. Inside Flatpak, which
//! sandboxes already, or with `SONORA_SANDBOX=0` nothing is installed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use landlock::{
    ABI, Access as _, AccessFs, LandlockStatus, RestrictSelfAttr as _, Ruleset, RulesetAttr as _,
    RulesetCreatedAttr as _, RulesetStatus, path_beneath_rules,
};

/// ABI v5 gates every filesystem right the kernel knows except pathname unix sockets (v9), so
/// wayland, pipewire and dbus keep working on any kernel new enough to enforce at all.
const ABI_VERSION: ABI = ABI::V5;
/// Set to 0 to run without the sandbox, the escape hatch for a rule that turns out too tight.
const DISABLE: &str = "SONORA_SANDBOX";
/// Flatpak writes this file into every sandbox it starts, and one sandbox nests poorly in
/// another.
const FLATPAK_INFO: &str = "/.flatpak-info";

/// The part of `settings.json` the sandbox reads on its own, since `state::Values` is private.
/// `local_folders` names the directories the tag editor writes back into, so they are the one
/// user-owned place besides the app's own that stays writable.
#[derive(serde::Deserialize)]
struct EarlySettings {
    #[serde(default)]
    local_folders: Vec<PathBuf>,
}

/// Installs the domain. `args` is the launch's non-flag arguments, the paths a cold "Open With"
/// hand-off names: any that exist get read access so they load before a restart.
pub fn apply(args: &[String]) {
    if std::env::var_os(DISABLE).is_some_and(|value| value == "0") {
        log::info!("sandbox: disabled by {DISABLE}=0");
        return;
    }
    if Path::new(FLATPAK_INFO).exists() {
        log::info!("sandbox: skipped inside flatpak");
        return;
    }

    match enforce(args) {
        Ok((status, count)) => match status.ruleset {
            RulesetStatus::FullyEnforced => {
                log::info!("sandbox: landlock enforced, {count} paths allowed");
            }
            RulesetStatus::PartiallyEnforced => {
                log::info!(
                    "sandbox: landlock partially enforced, {count} paths allowed{}",
                    dropped(&status.landlock)
                );
            }
            RulesetStatus::NotEnforced => {
                log::warn!("sandbox: landlock not enforced by this kernel, running unsandboxed");
            }
        },
        Err(error) => log::warn!("sandbox: landlock not applied: {error:#}"),
    }
}

/// Builds the ruleset and enforces it, returning the enforcement status and how many of the
/// candidate paths actually became rules.
fn enforce(args: &[String]) -> Result<(landlock::RestrictionStatus, usize)> {
    // A directory rule only covers what sits beneath the opened fd, so the app directories are
    // created first to exist on a fresh install.
    let writable = writable();
    let readable = readable(args);
    let write = AccessFs::from_all(ABI_VERSION) & !AccessFs::Execute;
    let read = AccessFs::from_read(ABI_VERSION);

    // Unopenable paths, meaning the optional and missing ones, never become rules.
    let write_rules: Vec<_> = path_beneath_rules(&writable, write).collect();
    let read_rules: Vec<_> = path_beneath_rules(&readable, read).collect();
    let count = write_rules.len() + read_rules.len();

    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI_VERSION))
        .context("cannot declare the handled rights")?
        .create()
        .context("cannot create the ruleset")?
        .add_rules(read_rules)
        .context("cannot allow the readable paths")?
        .add_rules(write_rules)
        .context("cannot allow the writable paths")?
        .all_threads(true)
        .context("cannot mark the domain process-wide")?
        .restrict_self()
        .context("cannot enforce the domain")?;

    Ok((status, count))
}

/// The directories the process may write: its own XDG folders, the runtime dir and tmp, the
/// shader and font caches, and the configured local music folders. Everything but `Execute`,
/// since nothing in them is ever run.
fn writable() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = [
        dirs::config_dir(),
        dirs::data_dir(),
        dirs::cache_dir(),
        dirs::state_dir(),
    ]
    .into_iter()
    .flatten()
    .map(|root| root.join("sonora"))
    .collect();
    for path in &paths {
        if let Err(error) = fs::create_dir_all(path) {
            log::warn!("sandbox: cannot create {}: {error}", path.display());
        }
    }

    paths.extend(dirs::runtime_dir());
    paths.push(PathBuf::from("/dev"));
    paths.push(PathBuf::from("/tmp"));

    if let Some(cache) = dirs::cache_dir() {
        paths.push(cache.join("fontconfig"));
        // Mesa names its shader cache per driver and arch (mesa_shader_cache_multiarch and
        // friends), so every existing sibling under the cache dir is picked up.
        if let Ok(entries) = fs::read_dir(&cache) {
            paths.extend(
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name().is_some_and(|name| {
                            name.to_string_lossy().starts_with("mesa_shader_cache")
                        })
                    }),
            );
        }
    }

    paths.extend(local_folders());
    paths
}

/// The directories the process may only read and execute: system libraries, fonts, icons and
/// resolver files, proc and sysfs, pulse cookie files, the places music folders usually land,
/// and whatever path a launch argument named.
fn readable(args: &[String]) -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/opt"),
        PathBuf::from("/etc"),
        PathBuf::from("/proc"),
        PathBuf::from("/sys"),
        PathBuf::from("/var/cache/fontconfig"),
        PathBuf::from("/var/lib/dbus"),
        PathBuf::from("/nix"),
        PathBuf::from("/media"),
        PathBuf::from("/run/media"),
        PathBuf::from("/mnt"),
    ];

    if let Some(home) = dirs::home_dir() {
        for name in [
            ".config/fontconfig",
            ".config/gtk-3.0",
            ".config/pulse",
            ".icons",
            ".local/share/fonts",
            ".local/share/icons",
            ".local/share/mime",
            ".pulse-cookie",
            ".asoundrc",
            ".Xauthority",
        ] {
            paths.push(home.join(name));
        }
    }

    paths.extend(dirs::audio_dir());
    paths.extend(dirs::download_dir());
    paths.extend(
        args.iter()
            .filter_map(|arg| crate::local_path_from_arg(arg))
            .filter(|path| path.exists()),
    );
    paths
}

/// The `local_folders` array of `settings.json`, or nothing where the file is absent, unreadable
/// or broken. `state::AppSettings` cannot be asked: it does not exist until the app runs, which
/// is after the domain must be installed.
fn local_folders() -> Vec<PathBuf> {
    let Some(path) = dirs::config_dir().map(|dir| dir.join("sonora").join("settings.json")) else {
        return Vec::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    match serde_json::from_slice::<EarlySettings>(&bytes) {
        Ok(settings) => settings.local_folders,
        Err(error) => {
            log::warn!("sandbox: cannot parse {}: {error}", path.display());
            Vec::new()
        }
    }
}

/// The rights the running kernel silently dropped from the request, for a partially enforced
/// domain's log line.
fn dropped(landlock: &LandlockStatus) -> String {
    let LandlockStatus::Available { effective_abi, .. } = landlock else {
        return String::new();
    };
    let missing = AccessFs::from_all(ABI_VERSION) & !AccessFs::from_all(*effective_abi);
    match missing.is_empty() {
        true => String::new(),
        false => format!(", rights without kernel support: {missing:?}"),
    }
}
