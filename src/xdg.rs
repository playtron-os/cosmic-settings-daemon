// Copyright 2025 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

//! Locating shared data files (sound themes, GTK themes) across install layouts.
//!
//! `/usr/share` only exists on FHS distributions. On Nix-style systems the same
//! files live under per-package store prefixes and are reached through
//! `XDG_DATA_DIRS`, or relative to the running executable. Searching those in
//! order keeps FHS behaviour identical, because `/usr/local/share:/usr/share`
//! is the XDG spec default when `XDG_DATA_DIRS` is unset.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// XDG Base Directory spec default for an unset `XDG_DATA_DIRS`.
const DEFAULT_DATA_DIRS: &str = "/usr/local/share:/usr/share";

/// Inputs that data lookup depends on.
///
/// Passed explicitly rather than read inline so tests can describe a layout
/// without mutating process-global environment variables.
#[derive(Debug, Default)]
pub struct DataEnv {
    /// `XDG_DATA_HOME`, or its `~/.local/share` default.
    pub data_home: Option<PathBuf>,
    /// Raw `XDG_DATA_DIRS`; `None` falls back to the spec default.
    pub data_dirs: Option<OsString>,
    /// Prefix the running executable was installed under.
    pub install_prefix: Option<PathBuf>,
}

impl DataEnv {
    /// Read the layout of the process we are running in.
    fn from_process() -> Self {
        Self {
            data_home: dirs::data_dir(),
            data_dirs: std::env::var_os("XDG_DATA_DIRS").filter(|dirs| !dirs.is_empty()),
            install_prefix: std::env::current_exe()
                .ok()
                .and_then(|exe| prefix_from_exe(&exe)),
        }
    }
}

/// `<prefix>/bin/cosmic-settings-daemon` -> `<prefix>`.
fn prefix_from_exe(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    (bin.file_name()? == "bin")
        .then(|| bin.parent())?
        .map(Path::to_path_buf)
}

/// Directories holding shared data, highest priority first.
fn data_dirs(env: &DataEnv) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    let mut push = |dir: PathBuf| {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    if let Some(data_home) = env.data_home.clone() {
        push(data_home);
    }

    let data_dirs = env
        .data_dirs
        .clone()
        .unwrap_or_else(|| OsString::from(DEFAULT_DATA_DIRS));

    for dir in std::env::split_paths(&data_dirs) {
        push(dir);
    }

    // Last resort: the prefix we were installed under, so a relocated install
    // (a Nix store path, for instance) finds data with no environment at all.
    if let Some(prefix) = env.install_prefix.clone() {
        push(prefix.join("share"));
    }

    dirs
}

/// Directories holding shared data for this process, highest priority first.
pub fn search_dirs() -> Vec<PathBuf> {
    data_dirs(&DataEnv::from_process())
}

/// First existing `<data dir>/<relative>`, searched in priority order.
pub fn find(relative: impl AsRef<Path>) -> Option<PathBuf> {
    find_in(&DataEnv::from_process(), relative.as_ref(), &|path| {
        path.exists()
    })
}

/// [`find`] against an injected layout and existence check.
fn find_in(env: &DataEnv, relative: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    data_dirs(env)
        .into_iter()
        .map(|dir| dir.join(relative))
        .find(|path| exists(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(data_home: Option<&str>, data_dirs: Option<&str>, prefix: Option<&str>) -> DataEnv {
        DataEnv {
            data_home: data_home.map(PathBuf::from),
            data_dirs: data_dirs.map(OsString::from),
            install_prefix: prefix.map(PathBuf::from),
        }
    }

    fn only(expected: &'static str) -> impl Fn(&Path) -> bool {
        move |path| path == Path::new(expected)
    }

    #[test]
    fn fhs_defaults_when_xdg_data_dirs_is_unset() {
        let env = env(Some("/home/user/.local/share"), None, Some("/usr"));

        assert_eq!(
            data_dirs(&env),
            [
                "/home/user/.local/share",
                "/usr/local/share",
                // Deduplicated with the install prefix's `share`.
                "/usr/share",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn fhs_sound_theme_still_resolves() {
        let env = env(Some("/home/user/.local/share"), None, Some("/usr"));

        assert_eq!(
            find_in(
                &env,
                Path::new("sounds/Pop"),
                &only("/usr/share/sounds/Pop")
            ),
            Some(PathBuf::from("/usr/share/sounds/Pop"))
        );
    }

    #[test]
    fn nix_system_profile_is_searched() {
        let env = env(
            Some("/home/user/.local/share"),
            Some("/home/user/.nix-profile/share:/run/current-system/sw/share"),
            Some("/nix/store/00000000-cosmic-settings-daemon-1.4.1"),
        );

        // Spec behaviour: an explicit XDG_DATA_DIRS replaces the default.
        assert!(!data_dirs(&env).contains(&PathBuf::from("/usr/share")));

        assert_eq!(
            find_in(
                &env,
                Path::new("themes/adw-gtk3-dark"),
                &only("/run/current-system/sw/share/themes/adw-gtk3-dark")
            ),
            Some(PathBuf::from(
                "/run/current-system/sw/share/themes/adw-gtk3-dark"
            ))
        );
    }

    #[test]
    fn store_prefix_resolves_without_any_environment() {
        let env = env(
            None,
            None,
            Some("/nix/store/00000000-cosmic-settings-daemon"),
        );

        assert_eq!(
            find_in(
                &env,
                Path::new("sounds/Pop"),
                &only("/nix/store/00000000-cosmic-settings-daemon/share/sounds/Pop")
            ),
            Some(PathBuf::from(
                "/nix/store/00000000-cosmic-settings-daemon/share/sounds/Pop"
            ))
        );
    }

    #[test]
    fn user_data_home_takes_priority() {
        let env = env(Some("/home/user/.local/share"), None, Some("/usr"));

        assert_eq!(
            find_in(&env, Path::new("sounds/Pop"), &|_| true),
            Some(PathBuf::from("/home/user/.local/share/sounds/Pop"))
        );
    }

    #[test]
    fn missing_data_is_reported_as_missing() {
        let env = env(Some("/home/user/.local/share"), None, Some("/usr"));

        assert_eq!(find_in(&env, Path::new("sounds/Pop"), &|_| false), None);
    }

    #[test]
    fn empty_data_dirs_entries_are_skipped() {
        let env = env(None, Some("/run/current-system/sw/share::"), None);

        assert_eq!(
            data_dirs(&env),
            [PathBuf::from("/run/current-system/sw/share")]
        );
    }

    #[test]
    fn prefix_is_derived_from_a_bin_directory() {
        assert_eq!(
            prefix_from_exe(Path::new("/usr/bin/cosmic-settings-daemon")),
            Some(PathBuf::from("/usr"))
        );
        assert_eq!(
            prefix_from_exe(Path::new(
                "/nix/store/00000000-cosmic-settings-daemon/bin/cosmic-settings-daemon"
            )),
            Some(PathBuf::from("/nix/store/00000000-cosmic-settings-daemon"))
        );
        // A build-tree binary has no install prefix to speak of.
        assert_eq!(
            prefix_from_exe(Path::new(
                "/home/user/src/target/debug/cosmic-settings-daemon"
            )),
            None
        );
    }
}
