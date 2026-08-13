// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::process::Stdio;

use walkdir::WalkDir;

/// Plays an audio file.
pub fn play(path: &Path) {
    let _result = tokio::process::Command::new("pw-play")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .arg("--media-role")
        .arg("Notification")
        .arg(path)
        .spawn();
}

pub fn play_sound(theme: &'static str, sound: &'static str) {
    if let Some(path) = sound_path(theme, sound).or_else(|| sound_path("freedesktop", sound)) {
        play(&path);
    }
}

#[memoize::memoize]
fn sound_path(theme: &'static str, sound: &'static str) -> Option<PathBuf> {
    // Sound themes are shared data: search every XDG data directory rather than
    // only the FHS one, which does not exist on Nix-style systems.
    for dir in crate::xdg::search_dirs() {
        let entries = WalkDir::new(dir.join("sounds").join(theme))
            .follow_links(true)
            .into_iter()
            .filter_map(Result::ok);

        for entry in entries {
            let path = entry.path();
            if path.is_file() && path.file_stem().is_some_and(|stem| stem == sound) {
                return Some(path.to_owned());
            }
        }
    }

    None
}
