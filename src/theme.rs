// TODO later...
// If configured to, run scripts in XDG_DATA_DIR/dark-mode.d/ or XDG_DATA_DIR/light-mode.d/
// when the theme is set to auto-export color palette, write to gtk3 / gtk4 / kde / ... css files
// read config file for lat/long

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::bail;
use chrono::{DateTime, Days, Local};
use cosmic::config::CosmicTk;
use cosmic::theme::CosmicTheme;
use cosmic_config::{ConfigGet, ConfigSet, CosmicConfigEntry};
use cosmic_theme::{Theme, ThemeMode};

use geonames::GeoPosition;
use notify::Watcher;
use sunrise::{Coordinates, SolarDay, SolarEvent};
use tokio::time::Instant;
use tokio_stream::StreamExt;

#[derive(Debug)]
pub struct SunriseSunset {
    last_update: DateTime<Local>,
    sunrise: Instant,
    sunset: Instant,
    lat: f64,
    long: f64,
}

pub enum ThemeMsg {
    ThemeMode(String),
    /// true if dark
    Theme(bool),
    Tk(String),
}

impl SunriseSunset {
    pub fn new(lat: f64, long: f64, t: Option<DateTime<Local>>) -> anyhow::Result<Self> {
        let (system_t, instant_t, t) = if let Some(t) = t {
            let system_t = SystemTime::from(t);
            let system_now = SystemTime::now();
            let delta_t = system_t.duration_since(system_now)?;
            let instant_t = Instant::now()
                .checked_add(delta_t)
                .ok_or(anyhow::anyhow!("Could not calculate instant"))?;

            (system_t, instant_t, t)
        } else {
            (SystemTime::now(), Instant::now(), Local::now())
        };

        let naive_date = t.date_naive();
        let coords = Coordinates::new(lat, long).unwrap();
        let solar_day = SolarDay::new(coords, naive_date);
        let sunrise = solar_day.event_time(SolarEvent::Sunrise).timestamp();
        let sunset = solar_day.event_time(SolarEvent::Sunset).timestamp();

        let Some(sunrise) =
            UNIX_EPOCH.checked_add(std::time::Duration::from_secs(u64::try_from(sunrise)?))
        else {
            bail!("Failed to calculate sunrise time");
        };

        let Some(sunset) =
            UNIX_EPOCH.checked_add(std::time::Duration::from_secs(u64::try_from(sunset)?))
        else {
            bail!("Failed to calculate sunset time");
        };

        let st_to_instant = |now: SystemTime, st: SystemTime| -> anyhow::Result<Instant> {
            Ok(if st > now {
                instant_t
                    .checked_add(st.duration_since(now)?)
                    .ok_or(anyhow::anyhow!("Failed to convert system time to instant"))?
            } else {
                instant_t
                    .checked_sub(now.duration_since(st)?)
                    .ok_or(anyhow::anyhow!("Failed to convert system time to instant"))?
            })
        };

        Ok(Self {
            last_update: t,
            sunrise: st_to_instant(system_t, sunrise)?,
            sunset: st_to_instant(system_t, sunset)?,
            lat,
            long,
        })
    }

    pub fn is_dark(&self) -> anyhow::Result<bool> {
        if self.last_update.date_naive() != Local::now().date_naive() {
            bail!("SunriseSunset out of date");
        }

        let now = Instant::now();
        Ok(now < self.sunrise || now >= self.sunset)
    }

    pub fn next(&self) -> anyhow::Result<Instant> {
        let now = Instant::now();
        if self.sunrise.checked_duration_since(now).is_some() {
            Ok(self.sunrise)
        } else if self.sunset.checked_duration_since(now).is_some() {
            Ok(self.sunset)
        } else {
            bail!("SunriseSunset instants have already passed...");
        }
    }

    pub fn update_next(&mut self) -> anyhow::Result<Instant> {
        match self.next() {
            Ok(i) => Ok(i),
            Err(_) => {
                let Some(tomorrow) = self.last_update.checked_add_days(Days::new(1)) else {
                    bail!("Failed to calculate next date for theme auto-switch.");
                };
                *self = Self::new(self.lat, self.long, Some(tomorrow))?;
                self.next()
            }
        }
    }
}

pub async fn watch_theme(
    theme_mode_rx: &mut tokio::sync::mpsc::Receiver<ThemeMsg>,
    theme_cancel_rx: &mut tokio::sync::mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let mut override_until_next = false;

    let helper = ThemeMode::config()?;
    let mut theme_mode = match ThemeMode::get_entry(&helper) {
        Ok(t) => t,
        Err((errs, t)) => {
            for why in errs {
                log::error!("{why}");
            }
            t
        }
    };

    let tk_helper = CosmicTk::config()?;
    let mut tk = match CosmicTk::get_entry(&tk_helper) {
        Ok(t) => t,
        Err((errs, t)) => {
            for why in errs {
                if let cosmic_config::Error::GetKey(_, err) = &why
                    && err.kind() == std::io::ErrorKind::NotFound
                {
                    // No system default config installed; don't error
                    continue;
                }
                log::error!("{why}");
            }
            t
        }
    };

    set_gnome_button_layout(tk.show_maximize, tk.show_minimize);
    set_gnome_icon_theme(tk.icon_theme.clone());

    let light_helper = CosmicTheme::light_config()?;
    let dark_helper = CosmicTheme::dark_config()?;

    // Pin the `is_dark` flag on the light/dark theme palette configs. cosmic-config
    // does not always persist `is_dark` when a theme is written, so it loads stale:
    // on a non-GNOME desktop a missing `is_dark` defaults to dark even for the light
    // theme. That makes xdg-desktop-portal publish the wrong `color-scheme` and other
    // cosmic apps mis-detect light/dark. The light theme is always light and the dark
    // theme always dark, so correct the flag when it is missing or wrong. Only write
    // on an actual change to avoid spurious config-change notifications (`set` always
    // writes) and any watcher churn.
    for (helper, expected) in [(&light_helper, false), (&dark_helper, true)] {
        if helper.get::<bool>("is_dark").ok() != Some(expected) {
            if let Err(err) = helper.set("is_dark", expected) {
                log::error!("Failed to pin is_dark={expected} on theme config: {err}");
            }
        }
    }

    if tk.apply_theme_global {
        crate::icetron::export();
        // Write the exports for both themes in case they have changed in the meantime
        let dark = match Theme::get_entry(&dark_helper) {
            Ok(t) => t,
            Err((errs, t)) => {
                for why in errs {
                    log::error!("{why}");
                }
                t
            }
        };
        _ = dark.write_exports();
        let light = match Theme::get_entry(&light_helper) {
            Ok(t) => t,
            Err((errs, t)) => {
                for why in errs {
                    log::error!("{why}");
                }
                t
            }
        };
        _ = light.write_exports();
        set_flatpak_overrides();

        if !theme_mode.auto_switch {
            let t = if theme_mode.is_dark { dark } else { light };
            if let Err(err) = apply_exports(&t) {
                log::error!("Failed to apply COSMIC theme exports. {err:?}");
            }
        }

        set_gnome_desktop_interface(theme_mode.is_dark);
    } else if let Err(err) = reset_exports() {
        log::error!("Failed to reset the cosmic theme exports. {err:?}");
    }

    // `icetron-theme set` relinks the active theme here.
    let (icetron_tx, mut icetron_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut icetron_watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok_and(|event| !matches!(event.kind, notify::EventKind::Access(_))) {
                let _ = icetron_tx.send(());
            }
        })?;
    if let Some(dir) = crate::icetron::config_dir()
        && let Err(err) = icetron_watcher.watch(&dir, notify::RecursiveMode::NonRecursive)
    {
        log::debug!("Not watching {}: {err}", dir.display());
    }

    // TODO allow preference for config file instead?
    let geodata = crate::location::decode_geodata();
    let (_location_handle, location_updates) = crate::location::receive_timezones();
    futures::pin_mut!(location_updates);

    let (_time_handle, time_changes) = crate::time::watch_time_changes().await?;
    futures::pin_mut!(time_changes);

    // Track the most-recent coordinates so we can recompute sunrise/sunset after suspend or
    // wall-clock changes.
    let mut coords: Option<(f64, f64)> = None;
    let mut sunrise_sunset: Option<SunriseSunset> = None;
    loop {
        let sunset_deadline =
            if let Some(Some(s)) = theme_mode.auto_switch.then_some(sunrise_sunset.as_mut()) {
                Some(s.update_next()?)
            } else {
                None
            };

        let sleep = async move {
            if !theme_mode.auto_switch {
                std::future::pending().await
            } else if let Some(s) = sunset_deadline {
                tokio::time::sleep_until(s).await
            } else {
                std::future::pending().await
            }
        };

        tokio::select! {
            _ = theme_cancel_rx.recv() => {
                return Ok(());
            }

            Some(()) = icetron_rx.recv() => {
                // One change comes as several events.
                while icetron_rx.try_recv().is_ok() {}
                if tk.apply_theme_global {
                    crate::icetron::export();
                    if let Err(err) = apply_gtk(theme_mode.is_dark) {
                        log::error!("Failed to apply the icetron theme to gtk. {err:?}");
                    }
                }
            }

            changes = theme_mode_rx.recv() => {
                let Some(changes) = changes else {
                    bail!("Theme mode changes failed");
                };

                match changes {
                    ThemeMsg::ThemeMode(changes) => {
                        let auto_switch_prev = theme_mode.auto_switch;

                        let (errs, _) = theme_mode.update_keys(&helper, &[changes]);

                        for err in errs {
                            log::error!("Error updating the theme mode {err:?}");
                        }

                        override_until_next = sunrise_sunset.as_ref().is_some_and(|s| s.is_dark().is_ok_and(|s_is_dark| s_is_dark != theme_mode.is_dark));

                        if theme_mode.auto_switch && !auto_switch_prev {
                            let Some(is_dark) = sunrise_sunset.as_ref().and_then(|s| s.is_dark().ok()) else {
                                continue;
                            };

                            if let Err(err) = theme_mode.set_is_dark(&helper, is_dark) {
                                log::error!("Failed to update theme mode {err:?}");
                            }
                        }

                        if tk.apply_theme_global {
                            let theme = match if theme_mode.is_dark {
                                Theme::get_entry(&dark_helper)
                            } else {
                                Theme::get_entry(&light_helper)
                            } {
                                Ok(t) => t,
                                Err((errs, t)) => {
                                    for err in errs {
                                        log::error!("{err}");
                                    }
                                    t
                                }
                            };

                            if let Err(err) = apply_exports(&theme) {
                                log::error!("Failed to apply COSMIC theme exports. {err:?}");
                            }

                            set_gnome_desktop_interface(theme_mode.is_dark);
                        }
                    },
                    ThemeMsg::Tk(changes) => {
                        let (errs, changes) = tk.update_keys(&tk_helper, &[changes]);

                        for err in errs {
                            log::error!("Error updating the theme toolkit config {err:?}");
                        }

                        if changes.contains(&"icon_theme") {
                            set_gnome_icon_theme(tk.icon_theme.clone());
                        }

                        if changes.contains(&"show_maximize") || changes.contains(&"show_minimize") {
                            set_gnome_button_layout(tk.show_maximize, tk.show_minimize);
                        }

                        if !changes.contains(&"apply_theme_global") {
                            continue;
                        }

                        if tk.apply_theme_global {
                            crate::icetron::export();
                            // Write the exports for both themes in case they have changed in the meantime
                            let dark = match Theme::get_entry(&dark_helper) {
                                Ok(t) => t,
                                Err((errs, t)) => {
                                    for why in errs {
                                        log::error!("{why}");
                                    }
                                    t
                                }
                            };
                            _ = dark.write_exports();
                            let light = match Theme::get_entry(&light_helper) {
                                Ok(t) => t,
                                Err((errs, t)) => {
                                    for why in errs {
                                        log::error!("{why}");
                                    }
                                    t
                                }
                            };
                            _ = light.write_exports();
                            set_flatpak_overrides();

                            let t = if theme_mode.is_dark { dark } else { light };
                            if let Err(err) = apply_exports(&t) {
                                log::error!("Failed to apply COSMIC theme exports. {err:?}");
                            }

                            set_gnome_desktop_interface(theme_mode.is_dark);
                        } else if let Err(err) = reset_exports() {
                            log::error!("Failed to reset the cosmic theme exports. {err:?}");
                        }
                    },
                    ThemeMsg::Theme(is_dark) => {
                        let t = match Theme::get_entry(if is_dark {
                                &dark_helper
                            } else {
                                &light_helper
                            }) {
                                Ok(t) => t,
                                Err((errs, t)) => {
                                    for err in errs {
                                        log::error!("Failed to load the theme. {err:?}");
                                    }
                                    t
                                },
                            };
                        if tk.apply_theme_global {
                            if let Err(err) = t.write_exports() {
                                log::error!("Failed to write COSMIC theme exports. {err:?}");
                            }
                            let theme_mode = match ThemeMode::get_entry(&helper) {
                                Ok(t) => t,
                                Err((errs, t)) => {
                                    for err in errs {
                                        log::error!("Failed to load the theme mode. {err:?}");
                                    }
                                    t
                                },
                            };
                            if theme_mode.is_dark == is_dark
                                && let Err(err) = apply_exports(&t) {
                                    log::error!("Failed to apply COSMIC theme exports. {err:?}");
                                }

                            set_gnome_desktop_interface(theme_mode.is_dark);
                        }
                    }
                }


            }
            _ = sleep => {
                if !theme_mode.auto_switch || override_until_next {
                    override_until_next = false;
                    continue;
                }
                // update the theme mode
                let Some(is_dark) = sunrise_sunset.as_ref().and_then(|s| s.is_dark().ok()) else {
                    continue;
                };

                if let Err(err) = theme_mode.set_is_dark(&helper, is_dark) {
                    log::error!("Failed to update theme mode {err:?}");
                }
                if tk.apply_theme_global {
                    let theme = match if theme_mode.is_dark {
                        Theme::get_entry(&dark_helper)
                    } else {
                        Theme::get_entry(&light_helper)
                    } {
                        Ok(t) => t,
                        Err((errs, t)) => {
                            for err in errs {
                                log::error!("{err}");
                            }
                            t
                        }
                    };
                    if let Err(err) = apply_exports(&theme) {
                        log::error!("Failed to apply COSMIC theme exports. {err:?}");
                    }

                    set_gnome_desktop_interface(theme_mode.is_dark);
                }
            }
            location_update = location_updates.next() => {
                if override_until_next {
                    continue;
                }
                // set the next timer
                // update the theme if necessary
                let Some(location_result) = location_update else {
                    continue;
                };

                let Ok(new_timezone) = location_result else {
                    continue;
                };

                let Some(&GeoPosition { latitude, longitude }) = geodata.get(&new_timezone) else {
                    log::error!("no matching geodata for {new_timezone}");
                    continue;
                };

                coords = Some((latitude, longitude));
                match SunriseSunset::new(latitude, longitude, None) {
                    Ok(s) => {
                        sunrise_sunset = Some(s);
                    },
                    Err(err) => {
                        log::error!("Failed to calculate sunrise and sunset for current location {err:?}");
                        sunrise_sunset = None;
                        continue;
                    },
                };

                if !theme_mode.auto_switch {
                    continue;
                }

                let Some(is_dark) = sunrise_sunset.as_ref().unwrap().is_dark().ok() else {
                    continue;
                };

                if let Err(err) = theme_mode.set_is_dark(&helper, is_dark) {
                    log::error!("Failed to update theme mode {err:?}");
                }
                if tk.apply_theme_global {
                    let theme = match if theme_mode.is_dark {
                        Theme::get_entry(&dark_helper)
                    } else {
                        Theme::get_entry(&light_helper)
                    } {
                        Ok(t) => t,
                        Err((errs, t)) => {
                            for err in errs {
                                log::error!("{err}");
                            }
                            t
                        }
                    };
                    if let Err(err) = apply_exports(&theme) {
                        log::error!("Failed to apply COSMIC theme exports. {err:?}");
                    }

                    set_gnome_desktop_interface(theme_mode.is_dark);
                }
            }
            time_change = time_changes.next() => {
                let Some(time_change) = time_change else {
                    continue;
                };

                // Suspend/resume and wall-clock steps (NTP, manual) do not advance tokio's
                // monotonic `Instant` the same way. Recompute sunrise/sunset instants so the next
                // sleep deadline and current day/night evaluation match wall-clock time.
                let Some((latitude, longitude)) = coords else {
                    continue;
                };

                match SunriseSunset::new(latitude, longitude, None) {
                    Ok(s) => sunrise_sunset = Some(s),
                    Err(err) => {
                        log::error!("Failed to recalculate sunrise/sunset after {time_change:?}: {err:?}");
                        sunrise_sunset = None;
                        continue;
                    }
                };

                // If auto-switch isn't enabled, keep the timer state fresh and bail.
                if !theme_mode.auto_switch {
                    continue;
                }

                let Some(is_dark) = sunrise_sunset.as_ref().and_then(|s| s.is_dark().ok()) else {
                    continue;
                };

                // Expire manual override once the schedule catches up (important if we slept
                // across the boundary).
                if override_until_next && is_dark == theme_mode.is_dark {
                    override_until_next = false;
                }

                if override_until_next {
                    continue;
                }

                if theme_mode.is_dark != is_dark {
                    if let Err(err) = theme_mode.set_is_dark(&helper, is_dark) {
                        log::error!("Failed to update theme mode after {time_change:?}: {err:?}");
                        continue;
                    }

                    if tk.apply_theme_global {
                        let theme = match if theme_mode.is_dark {
                            Theme::get_entry(&dark_helper)
                        } else {
                            Theme::get_entry(&light_helper)
                        } {
                            Ok(t) => t,
                            Err((errs, t)) => {
                                for err in errs {
                                    log::error!("{err}");
                                }
                                t
                            }
                        };
                        if let Err(err) = apply_gtk(theme.is_dark) {
                            log::error!("Failed to apply the theme to gtk. {err:?}");
                        }

                        set_gnome_desktop_interface(theme_mode.is_dark);
                    }
                }
            }

        }
    }
}

/// COSMIC's GTK and Qt exports for a theme, with GTK taken from the icetron
/// theme when one is exported.
fn apply_exports(theme: &Theme) -> anyhow::Result<()> {
    let gtk = apply_gtk(theme.is_dark);
    let qt = Theme::apply_qt(theme.is_dark);
    let qt56ct = Theme::apply_qt56ct(theme.is_dark);
    gtk?;
    qt?;
    qt56ct?;
    Ok(())
}

fn apply_gtk(is_dark: bool) -> anyhow::Result<()> {
    if let Some(css) = crate::icetron::css(is_dark) {
        crate::icetron::apply(&css)?;
        return Ok(());
    }
    crate::icetron::unlink()?;
    Theme::apply_gtk(is_dark)?;
    Ok(())
}

fn reset_exports() -> anyhow::Result<()> {
    crate::icetron::reset()?;
    Theme::reset_exports()?;
    Ok(())
}

fn set_gnome_button_layout(show_maximize: bool, show_minimize: bool) {
    let layout = match (show_maximize, show_minimize) {
        (true, true) => ":minimize,maximize,close",
        (true, false) => ":maximize,close",
        (false, true) => ":minimize,close",
        (false, false) => ":close",
    };
    tokio::spawn(set_gnome_key(
        "org.gnome.desktop.wm.preferences",
        "button-layout",
        layout.to_string(),
    ));
}

fn set_gnome_desktop_interface(is_dark: bool) {
    let (color_scheme, adw_theme) = if is_dark {
        ("prefer-dark", "adw-gtk3-dark")
    } else {
        ("prefer-light", "adw-gtk3")
    };

    tokio::spawn(set_gnome_key(
        INTERFACE,
        "color-scheme",
        color_scheme.to_string(),
    ));

    // GTK themes are shared data; look through every XDG data directory instead
    // of assuming the FHS one exists.
    if crate::xdg::find(format!("themes/{adw_theme}")).is_some() {
        tokio::spawn(set_gnome_key(INTERFACE, "gtk-theme", adw_theme.to_string()));
    }
}

fn set_gnome_icon_theme(theme: String) {
    tokio::spawn(set_gnome_key(INTERFACE, "icon-theme", theme));
}

const INTERFACE: &str = "org.gnome.desktop.interface";

/// Set a GNOME desktop key where GTK reads it. `gsettings` needs the schema
/// installed, and NixOS keeps none in the system profile, so fall back to
/// `dconf`, which writes the key without one.
async fn set_gnome_key(schema: &'static str, key: &'static str, value: String) {
    if !set_gnome_key_with("gsettings", "dconf", schema, key, &value).await {
        log::warn!("Failed to set {schema} {key} to {value}");
    }
}

async fn set_gnome_key_with(
    gsettings: &str,
    dconf: &str,
    schema: &str,
    key: &str,
    value: &str,
) -> bool {
    let set = tokio::process::Command::new(gsettings)
        .args(["set", schema, key, value])
        .output()
        .await;
    if set.is_ok_and(|out| out.status.success()) {
        return true;
    }
    tokio::process::Command::new(dconf)
        .args(["write", &dconf_path(schema, key), &gvariant_string(value)])
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// The dconf path of a key in a GNOME schema with no path of its own.
fn dconf_path(schema: &str, key: &str) -> String {
    format!("/{}/{key}", schema.replace('.', "/"))
}

/// `value` as a GVariant string literal.
fn gvariant_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn set_flatpak_overrides() {
    let paths_to_expose = vec![
        "xdg-config/gtk-4.0:ro",
        "xdg-config/gtk-3.0:ro",
        "xdg-config/kdeglobals:ro",
        "xdg-data/color-schemes:ro",
    ];

    tokio::spawn(async {
        // Unset incorrectly-defined platform theme.
        if let Some(mut overrides_path) = std::env::home_dir() {
            overrides_path = overrides_path.join(".local/share/flatpak/overrides/global");
            if let Ok(mut overrides) = tokio::fs::read_to_string(&overrides_path).await {
                overrides = overrides.replace("\nQT_QPA_PLATFORMTHEME=kde", "");
                _ = tokio::fs::write(&overrides_path, overrides.as_bytes()).await;
            }
        }

        for path in paths_to_expose {
            _ = tokio::process::Command::new("flatpak")
                .arg("override")
                .arg("--user")
                .arg("--filesystem")
                .arg(path)
                .status()
                .await;
        }
    });
}

#[cfg(test)]
mod gnome_key_tests {
    use super::*;

    #[test]
    fn a_key_lands_at_its_schema_path() {
        assert_eq!(
            dconf_path(INTERFACE, "gtk-theme"),
            "/org/gnome/desktop/interface/gtk-theme"
        );
        assert_eq!(
            dconf_path("org.gnome.desktop.wm.preferences", "button-layout"),
            "/org/gnome/desktop/wm/preferences/button-layout"
        );
    }

    /// A `gsettings` that fails as it does with no schemas, and a `dconf`
    /// that records what it was asked to write.
    #[test]
    fn a_key_goes_to_dconf_when_gsettings_has_no_schema() {
        let dir = std::env::temp_dir().join(format!("csd-gnome-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path.to_string_lossy().into_owned()
        };
        let written = dir.join("written");
        let gsettings = script("gsettings", "echo 'No schemas installed' >&2; exit 1");
        let dconf = script(
            "dconf",
            &format!("printf '%s\\n' \"$@\" > {}", written.display()),
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let done = runtime.block_on(set_gnome_key_with(
            &gsettings,
            &dconf,
            INTERFACE,
            "gtk-theme",
            "adw-gtk3-dark",
        ));
        let args = std::fs::read_to_string(&written).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(done);
        assert_eq!(
            args,
            "write\n/org/gnome/desktop/interface/gtk-theme\n'adw-gtk3-dark'\n"
        );
    }

    #[test]
    fn values_are_written_as_gvariant_strings() {
        assert_eq!(gvariant_string("adw-gtk3-dark"), "'adw-gtk3-dark'");
        assert_eq!(gvariant_string(r"it's a \ name"), r"'it\'s a \\ name'");
    }
}
