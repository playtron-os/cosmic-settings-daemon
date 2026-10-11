// SPDX-License-Identifier: GPL-3.0-only

//! GTK styling from the active icetron theme, the one Kora's own apps are drawn
//! from. Its colors are written as libadwaita's named colors to
//! `~/.config/gtk-4.0/icetron/`, the same files and mapping as icetron's own
//! GTK export, and GTK apps are linked to them rather than to the COSMIC
//! palette. Without an icetron theme nothing here does anything.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `~/.config/icetron`, whose `current-theme` names the active theme.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("icetron"))
}

/// Where the GTK CSS goes: `dark.css` and `light.css`.
fn export_dir(config: &Path) -> PathBuf {
    config.join("gtk-4.0").join("icetron")
}

/// The active theme's directory, found as icetron finds it: the user's
/// `current-theme`, the administrator's, then the data directories'.
fn theme_dir() -> Option<PathBuf> {
    let user = config_dir().map(|dir| dir.join("current-theme"));
    let admin = Some(PathBuf::from("/etc/icetron/current-theme"));
    let data = crate::xdg::find("icetron/current-theme");
    [user, admin, data]
        .into_iter()
        .flatten()
        .filter_map(|link| std::fs::canonicalize(link).ok())
        .find(|dir| dir.is_dir())
}

/// The tokens of a theme's `dark.ron` or `light.ron` that GTK is given.
#[derive(Deserialize)]
#[serde(rename = "DynamicTheme")]
struct Tokens {
    primary: Color,
    primary_foreground: Color,
    background: Color,
    foreground: Color,
    surface_1: Color,
    surface_elevated: Color,
    card: Color,
    fill_popup: Color,
    stroke_normal: Color,
    stroke_subtle: Color,
    stroke_subtlest: Color,
    feedback_success_primary: Color,
    feedback_success_tertiary: Color,
    feedback_caution_primary: Color,
    feedback_caution_tertiary: Color,
    feedback_error_primary: Color,
    feedback_error_tertiary: Color,
    state_default_danger: Color,
}

#[derive(Deserialize, Clone, Copy)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    fn css(self) -> String {
        let [r, g, b] = [self.r, self.g, self.b].map(|c| (c * 255.0).round() as u8);
        if (self.a - 1.0).abs() < f32::EPSILON {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("rgba({r}, {g}, {b}, {:.3})", self.a)
        }
    }
}

impl Tokens {
    /// libadwaita's named colors, as icetron's GTK export defines them.
    fn gtk_css(&self) -> String {
        let colors = [
            ("accent_color", self.primary),
            ("accent_bg_color", self.primary),
            ("accent_fg_color", self.primary_foreground),
            ("window_bg_color", self.background),
            ("window_fg_color", self.foreground),
            ("headerbar_bg_color", self.surface_1),
            ("headerbar_fg_color", self.foreground),
            ("headerbar_border_color", self.stroke_subtle),
            ("headerbar_backdrop_color", self.surface_1),
            ("headerbar_shade_color", self.stroke_subtlest),
            ("card_bg_color", self.card),
            ("card_fg_color", self.foreground),
            ("card_shade_color", self.stroke_subtlest),
            ("popover_bg_color", self.fill_popup),
            ("popover_fg_color", self.foreground),
            ("popover_shade_color", self.stroke_subtlest),
            ("dialog_bg_color", self.surface_elevated),
            ("dialog_fg_color", self.foreground),
            ("sidebar_bg_color", self.surface_1),
            ("sidebar_fg_color", self.foreground),
            ("sidebar_shade_color", self.stroke_subtlest),
            ("sidebar_backdrop_color", self.surface_1),
            ("view_bg_color", self.background),
            ("view_fg_color", self.foreground),
            ("success_color", self.feedback_success_primary),
            ("success_bg_color", self.feedback_success_tertiary),
            ("warning_color", self.feedback_caution_primary),
            ("warning_bg_color", self.feedback_caution_tertiary),
            ("error_color", self.feedback_error_primary),
            ("error_bg_color", self.feedback_error_tertiary),
            ("destructive_color", self.feedback_error_primary),
            ("destructive_bg_color", self.state_default_danger),
            ("borders", self.stroke_normal),
            ("shade_color", self.stroke_subtlest),
            ("scrollbar_outline_color", self.stroke_subtle),
        ];
        let mut css = String::from("/* Generated from the active icetron theme */\n\n");
        for (name, color) in colors {
            css.push_str(&format!("@define-color {name} {};\n", color.css()));
        }
        css
    }
}

/// Write the active icetron theme's GTK CSS for both color modes; `false`
/// when there is no icetron theme to write it from.
pub fn export() -> bool {
    let (Some(theme), Ok(config)) = (theme_dir(), config()) else {
        return false;
    };
    match export_in(&theme, &config) {
        Ok(()) => true,
        Err(err) => {
            log::warn!(
                "Failed to export the icetron theme at {}: {err}",
                theme.display()
            );
            false
        }
    }
}

fn export_in(theme: &Path, config: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dir = export_dir(config);
    std::fs::create_dir_all(&dir)?;
    for mode in ["dark", "light"] {
        let ron = std::fs::read_to_string(theme.join(format!("{mode}.ron")))?;
        let tokens: Tokens = ron::from_str(&ron)?;
        let css = dir.join(format!("{mode}.css"));
        let tmp = dir.join(format!(".{mode}.css~"));
        std::fs::write(&tmp, tokens.gtk_css())?;
        std::fs::rename(&tmp, &css)?;
    }
    Ok(())
}

/// The exported CSS for a color mode, if there is one.
pub fn css(is_dark: bool) -> Option<PathBuf> {
    css_in(&dirs::config_dir()?, is_dark)
}

fn css_in(config: &Path, is_dark: bool) -> Option<PathBuf> {
    let css = export_dir(config).join(if is_dark { "dark.css" } else { "light.css" });
    css.is_file().then_some(css)
}

/// Link GTK 3 and 4's `gtk.css` to `css`. A `gtk.css` that is neither this
/// export's nor COSMIC's is kept as `gtk.css.bak`, as COSMIC's export keeps it.
pub fn apply(css: &Path) -> io::Result<()> {
    apply_in(&config()?, css)
}

fn apply_in(config: &Path, css: &Path) -> io::Result<()> {
    for dest in gtk_css(config) {
        std::fs::create_dir_all(dest.parent().unwrap_or(config))?;
        if links_into(&dest, &export_dir(config)) || links_into(&dest, &cosmic_dir(config)) {
            std::fs::remove_file(&dest)?;
        } else if dest.symlink_metadata().is_ok() {
            std::fs::rename(&dest, dest.with_extension("css.bak"))?;
        }
        std::os::unix::fs::symlink(css, &dest)?;
    }
    Ok(())
}

/// Remove the links to this export, leaving any backup for COSMIC's export,
/// which links `gtk.css` next and takes over the backup it finds.
pub fn unlink() -> io::Result<()> {
    unlink_in(&config()?)
}

fn unlink_in(config: &Path) -> io::Result<()> {
    for dest in gtk_css(config) {
        if links_into(&dest, &export_dir(config)) {
            std::fs::remove_file(&dest)?;
        }
    }
    Ok(())
}

/// Undo [`apply`]: remove the links to this export and restore a kept `gtk.css`.
pub fn reset() -> io::Result<()> {
    reset_in(&config()?)
}

fn reset_in(config: &Path) -> io::Result<()> {
    for dest in gtk_css(config) {
        if !links_into(&dest, &export_dir(config)) {
            continue;
        }
        std::fs::remove_file(&dest)?;
        let backup = dest.with_extension("css.bak");
        if backup.symlink_metadata().is_ok() {
            std::fs::rename(&backup, &dest)?;
        }
    }
    Ok(())
}

fn config() -> io::Result<PathBuf> {
    dirs::config_dir().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
}

fn gtk_css(config: &Path) -> [PathBuf; 2] {
    ["gtk-4.0", "gtk-3.0"].map(|gtk| config.join(gtk).join("gtk.css"))
}

/// Where COSMIC's own GTK export lives.
fn cosmic_dir(config: &Path) -> PathBuf {
    config.join("gtk-4.0").join("cosmic")
}

fn links_into(path: &Path, dir: &Path) -> bool {
    std::fs::read_link(path).is_ok_and(|target| target.starts_with(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty directory of the test's own, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("icetron-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn export(config: &Path) {
        let dir = export_dir(config);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dark.css"), "dark").unwrap();
        std::fs::write(dir.join("light.css"), "light").unwrap();
    }

    #[test]
    fn gtk_is_linked_to_the_export_and_a_users_css_is_kept() {
        let tmp = Scratch::new("linked");
        let config = tmp.0.as_path();
        export(config);
        std::fs::create_dir_all(config.join("gtk-3.0")).unwrap();
        std::fs::write(config.join("gtk-3.0/gtk.css"), "@import 'colors.css';").unwrap();

        let dark = css_in(config, true).unwrap();
        apply_in(config, &dark).unwrap();
        for gtk in ["gtk-4.0", "gtk-3.0"] {
            let css = config.join(gtk).join("gtk.css");
            assert_eq!(std::fs::read_link(&css).unwrap(), dark);
        }
        assert_eq!(
            std::fs::read_to_string(config.join("gtk-3.0/gtk.css.bak")).unwrap(),
            "@import 'colors.css';"
        );

        // A switch of mode relinks without touching the backup.
        let light = css_in(config, false).unwrap();
        apply_in(config, &light).unwrap();
        assert_eq!(
            std::fs::read_link(config.join("gtk-3.0/gtk.css")).unwrap(),
            light
        );
        assert!(config.join("gtk-3.0/gtk.css.bak").exists());

        reset_in(config).unwrap();
        assert_eq!(
            std::fs::read_to_string(config.join("gtk-3.0/gtk.css")).unwrap(),
            "@import 'colors.css';"
        );
        assert!(config.join("gtk-4.0/gtk.css").symlink_metadata().is_err());
    }

    #[test]
    fn cosmics_link_is_replaced_and_its_backup_left_alone() {
        let tmp = Scratch::new("cosmic");
        let config = tmp.0.as_path();
        export(config);
        let cosmic = cosmic_dir(config);
        std::fs::create_dir_all(&cosmic).unwrap();
        std::fs::write(cosmic.join("dark.css"), "cosmic").unwrap();
        std::fs::create_dir_all(config.join("gtk-3.0")).unwrap();
        std::os::unix::fs::symlink(cosmic.join("dark.css"), config.join("gtk-3.0/gtk.css"))
            .unwrap();
        std::fs::write(config.join("gtk-3.0/gtk.css.bak"), "user").unwrap();

        apply_in(config, &css_in(config, true).unwrap()).unwrap();
        assert!(links_into(
            &config.join("gtk-3.0/gtk.css"),
            &export_dir(config)
        ));
        assert_eq!(
            std::fs::read_to_string(config.join("gtk-3.0/gtk.css.bak")).unwrap(),
            "user"
        );

        // Handing back to COSMIC's export leaves it the backup to manage.
        unlink_in(config).unwrap();
        assert!(config.join("gtk-3.0/gtk.css").symlink_metadata().is_err());
        assert!(config.join("gtk-3.0/gtk.css.bak").exists());
    }

    #[test]
    fn a_themes_tokens_become_libadwaitas_colors() {
        let tmp = Scratch::new("export");
        let (theme, config) = (tmp.0.join("theme"), tmp.0.join("config"));
        std::fs::create_dir_all(&theme).unwrap();
        let color = |r: f32, a: f32| format!("Color(r: {r}, g: 0.0, b: 0.0, a: {a})");
        for mode in ["dark", "light"] {
            let mut ron = String::from("DynamicTheme(\n    text_styles: (xs: 12.0),\n");
            for field in [
                "primary_foreground",
                "background",
                "foreground",
                "surface_1",
                "surface_elevated",
                "card",
                "fill_popup",
                "stroke_normal",
                "stroke_subtle",
                "stroke_subtlest",
                "feedback_success_primary",
                "feedback_success_tertiary",
                "feedback_caution_primary",
                "feedback_caution_tertiary",
                "feedback_error_primary",
                "feedback_error_tertiary",
                "state_default_danger",
            ] {
                ron.push_str(&format!("    {field}: {},\n", color(0.0, 1.0)));
            }
            let primary = if mode == "dark" { 1.0 } else { 0.5 };
            ron.push_str(&format!("    primary: {},\n", color(primary, 1.0)));
            ron.push_str(&format!("    window_nav: Some({}),\n", color(1.0, 0.5)));
            ron.push_str(")\n");
            std::fs::write(theme.join(format!("{mode}.ron")), ron).unwrap();
        }

        export_in(&theme, &config).unwrap();
        let dark = std::fs::read_to_string(css_in(&config, true).unwrap()).unwrap();
        assert!(dark.contains("@define-color accent_color #ff0000;\n"));
        assert!(dark.contains("@define-color window_bg_color #000000;\n"));
        let light = std::fs::read_to_string(css_in(&config, false).unwrap()).unwrap();
        assert!(light.contains("@define-color accent_bg_color #800000;\n"));
        assert_eq!(
            Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.5
            }
            .css(),
            "rgba(255, 255, 255, 0.500)"
        );
    }

    #[test]
    fn without_an_export_there_is_no_css() {
        let tmp = Scratch::new("none");
        assert!(css_in(&tmp.0, true).is_none());
    }
}
