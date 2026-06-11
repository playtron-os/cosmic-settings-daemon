//! Automatic timezone: keep the system timezone aligned with the device's
//! geographic location.
//!
//! When the `auto_timezone` setting (in `com.system76.CosmicAppletTime`) is
//! enabled, this subsystem resolves the device's IANA timezone in two hops and
//! applies it to systemd-timedated:
//!   1. **Location** — `org.freedesktop.GeoClue2` (the system location daemon,
//!      which itself uses Wi-Fi / IP geolocation) gives us latitude/longitude.
//!   2. **Coordinate → zone** — the embedded [`tzf_rs`] boundary database maps
//!      those coordinates to an IANA name (e.g. `America/Toronto`). No network,
//!      no per-request third-party lookup once GeoClue has a fix.
//!
//! The Settings app already does this while its Date & Time page is open; the
//! daemon covers the common case where the app is closed, re-checking on a fixed
//! interval and on resume-from-suspend so a move (or a flight) converges on the
//! right zone without anything being open.

use std::time::Duration;

use tokio_stream::StreamExt as _;
use tzf_rs::DefaultFinder;
use zbus::zvariant::OwnedObjectPath;

use crate::time::{self, TimeChange};

/// Config component/key that the Settings UI toggle writes. Reading the same key
/// here means the daemon honors the user's Automatic-timezone switch.
const APPLET_TIME_CONFIG: &str = "com.system76.CosmicAppletTime";
const APPLET_TIME_VERSION: u64 = 1;
const AUTO_TIMEZONE_KEY: &str = "auto_timezone";

/// Desktop id reported to GeoClue (used by its agent for authorization/display).
const DESKTOP_ID: &str = "com.system76.CosmicSettingsDaemon";
/// GeoClue accuracy level. Must be above CITY (4): geoclue's Wi-Fi source only
/// *submits the scanned access points* — i.e. does real positioning instead of a
/// coarse IP-based guess — when the requested accuracy is NEIGHBORHOOD (5) or
/// higher. With CITY it returns the backend's IP fallback, which is frequently
/// the wrong region. We request EXACT (8) to force a real Wi-Fi fix; the precise
/// coordinates are only fed to the timezone lookup.
const ACCURACY_EXACT: u32 = 8;
/// How long to wait for GeoClue to refine to an accurate fix before giving up.
const LOCATION_TIMEOUT: Duration = Duration::from_secs(20);
/// Accuracy (meters) at or below which a fix is treated as real positioning
/// (Wi-Fi/GPS/cell) rather than a coarse IP guess. IP fixes report tens to
/// hundreds of km; Wi-Fi/cell are well under this. We stop waiting once a fix is
/// this good (and otherwise fall back to the best one seen before the timeout).
const GOOD_ACCURACY_M: f64 = 10_000.0;
/// How often to re-evaluate the geographic timezone while Automatic is on.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Delay before the first detection, to let the network and GeoClue settle after
/// session bring-up.
const STARTUP_DELAY: Duration = Duration::from_secs(15);

/// Whether the user has Automatic timezone enabled. Defaults to `true` (matching
/// the Settings app) when the key has never been written.
fn auto_timezone_enabled() -> bool {
    use cosmic_config::{Config, ConfigGet};
    match Config::new(APPLET_TIME_CONFIG, APPLET_TIME_VERSION) {
        Ok(config) => config.get::<bool>(AUTO_TIMEZONE_KEY).unwrap_or(true),
        Err(err) => {
            log::debug!("auto-timezone: reading {AUTO_TIMEZONE_KEY} failed ({err}); assuming on");
            true
        }
    }
}

/// Run the automatic-timezone loop for the lifetime of the daemon.
///
/// Spawned with `spawn_local` (it itself spawns local tasks via
/// [`time::watch_time_changes`]).
pub async fn monitor() {
    let conn = match zbus::Connection::system().await {
        Ok(conn) => conn,
        Err(err) => {
            log::error!("auto-timezone: system bus unavailable, disabling: {err:?}");
            return;
        }
    };

    // Building the finder loads the embedded boundary data, so do it once.
    let finder = DefaultFinder::new();

    // Re-detect on resume-from-suspend. We deliberately ignore wall-clock
    // changes: applying a timezone *is* a wall-clock change, so reacting to it
    // would loop. Detection still works without this stream (interval only).
    let resume = match time::watch_time_changes().await {
        Ok((watcher, stream)) => Some((watcher, stream)),
        Err(err) => {
            log::warn!("auto-timezone: resume detection unavailable: {err:?}");
            None
        }
    };
    // `_watcher` must outlive the loop: dropping it aborts the watch tasks.
    let (_watcher, mut resume_stream) = match resume {
        Some((watcher, stream)) => (Some(watcher), Some(stream)),
        None => (None, None),
    };

    tokio::time::sleep(STARTUP_DELAY).await;

    let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
    // The first `tick()` resolves immediately; consume it so the wait below only
    // fires on genuine interval boundaries.
    ticker.tick().await;

    loop {
        if auto_timezone_enabled() {
            match detect_and_apply(&conn, &finder).await {
                Ok(Some((tz, source))) => {
                    log::info!("auto-timezone: applied {tz} (via {source})")
                }
                Ok(None) => log::debug!("auto-timezone: zone already correct"),
                Err(err) => log::warn!("auto-timezone: {err}"),
            }
        }

        // Wait for the next interval tick or a resume event.
        'wait: loop {
            // `stream_ended` defers clearing `resume_stream` until after the
            // mutable borrow taken by `as_mut()` below has been released.
            let mut stream_ended = false;
            match resume_stream.as_mut() {
                Some(stream) => {
                    tokio::select! {
                        _ = ticker.tick() => break 'wait,
                        event = stream.next() => match event {
                            Some(TimeChange::Resume) => break 'wait,
                            // Wall-clock change (incl. our own SetTimezone): keep
                            // waiting without re-detecting.
                            Some(_) => {}
                            // Stream ended: fall back to interval-only.
                            None => stream_ended = true,
                        }
                    }
                }
                None => {
                    ticker.tick().await;
                    break 'wait;
                }
            }
            if stream_ended {
                resume_stream = None;
            }
        }
    }
}

/// One-shot detection for diagnostics (`cosmic-settings-daemon --detect-timezone`).
///
/// Returns a human-readable description of what was resolved (and via which
/// source) without touching the system clock, so it can be run alongside the
/// live daemon.
pub(crate) async fn detect_once(
    conn: &zbus::Connection,
    finder: &DefaultFinder,
) -> Result<String, String> {
    match detect_location(conn).await {
        Ok((latitude, longitude)) => {
            let tz = lookup_timezone(finder, latitude, longitude)?;
            Ok(format!(
                "source=geoclue location=({latitude}, {longitude}) timezone={tz}"
            ))
        }
        Err(geoclue_err) => match crate::offline_tz::detect().await {
            Some(tz) => Ok(format!(
                "source=offline-ip timezone={tz} (geoclue unavailable: {geoclue_err})"
            )),
            None => Err(format!(
                "geoclue unavailable ({geoclue_err}) and offline IP fallback found nothing"
            )),
        },
    }
}

/// Resolve the geographic timezone: GeoClue (precise, Wi-Fi) first, then the
/// offline IP-based fallback. Returns `(source, tz)`, or `None` if both fail.
async fn detect_zone(
    conn: &zbus::Connection,
    finder: &DefaultFinder,
) -> Option<(&'static str, String)> {
    match detect_location(conn).await {
        Ok((latitude, longitude)) => match lookup_timezone(finder, latitude, longitude) {
            Ok(tz) => return Some(("geoclue", tz)),
            Err(e) => log::debug!("auto-timezone: coordinate lookup failed: {e}"),
        },
        Err(e) => log::debug!("auto-timezone: geoclue unavailable: {e}"),
    }
    crate::offline_tz::detect()
        .await
        .map(|tz| ("offline-ip", tz))
}

/// Detect the geographic zone and apply it if it differs from the current one.
///
/// Returns `Ok(Some((tz, source)))` when a new zone was applied, `Ok(None)` when
/// the zone was already correct, and `Err` when no zone could be determined or
/// timedated refused the change.
async fn detect_and_apply(
    conn: &zbus::Connection,
    finder: &DefaultFinder,
) -> Result<Option<(String, &'static str)>, String> {
    let (source, tz) = detect_zone(conn, finder)
        .await
        .ok_or_else(|| "no fix from geoclue or the offline IP fallback".to_string())?;

    let timedate = TimedateProxy::new(conn)
        .await
        .map_err(|e| format!("timedated proxy failed: {e}"))?;

    let current = timedate.timezone().await.unwrap_or_default();
    if current == tz {
        return Ok(None);
    }

    // Non-interactive: a background service must never trigger a password
    // prompt. This requires polkit to allow set-timezone for the active local
    // session (see data/polkit-1/rules.d/cosmic-settings-daemon.rules).
    timedate
        .set_timezone(&tz, false)
        .await
        .map_err(|e| format!("set_timezone({tz}) failed (polkit?): {e}"))?;
    Ok(Some((tz, source)))
}

/// Map a coordinate to an IANA timezone name. `Err` for coordinates with no land
/// timezone (e.g. mid-ocean), which we treat as a failed detection.
fn lookup_timezone(
    finder: &DefaultFinder,
    latitude: f64,
    longitude: f64,
) -> Result<String, String> {
    // NOTE: tzf-rs takes (longitude, latitude) — lng first.
    let name = finder.get_tz_name(longitude, latitude);
    if name.is_empty() {
        Err(format!("no timezone for ({latitude}, {longitude})"))
    } else {
        Ok(name.to_string())
    }
}

/// Ask GeoClue2 for the device's current latitude/longitude.
async fn detect_location(conn: &zbus::Connection) -> Result<(f64, f64), String> {
    let manager = GeoClueManagerProxy::new(conn)
        .await
        .map_err(|e| format!("geoclue manager proxy failed: {e}"))?;

    let client_path = manager
        .get_client()
        .await
        .map_err(|e| format!("geoclue GetClient failed: {e}"))?;

    let client = GeoClueClientProxy::builder(conn)
        .path(client_path)
        .map_err(|e| format!("invalid geoclue client path: {e}"))?
        .build()
        .await
        .map_err(|e| format!("geoclue client proxy failed: {e}"))?;

    // GeoClue requires a desktop id before it will start, and an accuracy level
    // tells it (and its agent) how precise a fix to authorize.
    client
        .set_desktop_id(DESKTOP_ID)
        .await
        .map_err(|e| format!("geoclue set DesktopId failed: {e}"))?;
    client
        .set_requested_accuracy_level(ACCURACY_EXACT)
        .await
        .map_err(|e| format!("geoclue set accuracy failed: {e}"))?;

    // Subscribe before Start() so we can't miss the first fix.
    let mut updates = client
        .receive_location_updated()
        .await
        .map_err(|e| format!("geoclue signal subscribe failed: {e}"))?;

    client
        .start()
        .await
        .map_err(|e| format!("geoclue Start failed: {e}"))?;

    // GeoClue emits a fast, coarse IP-based fix first and refines to a real
    // Wi-Fi/GPS fix moments later. Keep the most accurate fix seen, stopping early
    // once one is clearly not IP-level (<= GOOD_ACCURACY_M), bounded by
    // LOCATION_TIMEOUT — otherwise we'd act on the wrong-region IP guess.
    let mut fix = BestFix::new();

    if let Ok(path) = client.location().await {
        if path.as_str() != "/" {
            if let Some((lat, lon, acc)) = read_location(conn, &path).await {
                fix.record(lat, lon, acc);
            }
        }
    }

    let deadline = tokio::time::Instant::now() + LOCATION_TIMEOUT;
    while !fix.is_good_enough() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let signal = match tokio::time::timeout(remaining, updates.next()).await {
            Ok(Some(signal)) => signal,
            Ok(None) | Err(_) => break,
        };
        let Ok(args) = signal.args() else { continue };
        if let Some((lat, lon, acc)) = read_location(conn, &args.new).await {
            fix.record(lat, lon, acc);
        }
    }

    // Best-effort: release the client so the daemon can stop polling.
    let _ = client.stop().await;

    match fix.coords() {
        Some((latitude, longitude)) => {
            log::debug!(
                "geoclue fix: ({latitude}, {longitude}) ~{:.0}m",
                fix.accuracy_m
            );
            Ok((latitude, longitude))
        }
        None => Err("geoclue produced no location fix".to_string()),
    }
}

/// Tracks the most accurate GeoClue fix seen across `LocationUpdated` events.
///
/// GeoClue emits a fast, coarse IP-based fix first and refines to a real
/// Wi-Fi/GPS fix moments later, so detection must keep the *best* fix rather than
/// the *first* one, and only stop waiting once a fix is clearly not IP-level.
struct BestFix {
    coords: Option<(f64, f64)>,
    /// Accuracy of `coords` in meters; `INFINITY` while empty.
    accuracy_m: f64,
}

impl BestFix {
    fn new() -> Self {
        Self {
            coords: None,
            accuracy_m: f64::INFINITY,
        }
    }

    /// Record a fix, keeping it only if it is more accurate than the best so far.
    fn record(&mut self, latitude: f64, longitude: f64, accuracy_m: f64) {
        if accuracy_m < self.accuracy_m {
            self.coords = Some((latitude, longitude));
            self.accuracy_m = accuracy_m;
        }
    }

    /// Whether the best fix is precise enough to stop waiting for refinements.
    fn is_good_enough(&self) -> bool {
        self.accuracy_m <= GOOD_ACCURACY_M
    }

    fn coords(&self) -> Option<(f64, f64)> {
        self.coords
    }
}

/// Read latitude, longitude, and accuracy (meters) from a GeoClue Location path.
async fn read_location(conn: &zbus::Connection, path: &OwnedObjectPath) -> Option<(f64, f64, f64)> {
    let location = GeoClueLocationProxy::builder(conn)
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    let latitude = location.latitude().await.ok()?;
    let longitude = location.longitude().await.ok()?;
    let accuracy = location.accuracy().await.unwrap_or(f64::INFINITY);
    Some((latitude, longitude, accuracy))
}

// ─── D-Bus proxy for org.freedesktop.timedate1 ───────────────────────────────

#[zbus::proxy(
    interface = "org.freedesktop.timedate1",
    default_service = "org.freedesktop.timedate1",
    default_path = "/org/freedesktop/timedate1"
)]
trait Timedate {
    #[zbus(property)]
    fn timezone(&self) -> zbus::Result<String>;

    fn set_timezone(&self, timezone: &str, interactive: bool) -> zbus::Result<()>;
}

// ─── D-Bus proxies for org.freedesktop.GeoClue2 ──────────────────────────────

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Manager",
    default_service = "org.freedesktop.GeoClue2",
    default_path = "/org/freedesktop/GeoClue2/Manager"
)]
trait GeoClueManager {
    fn get_client(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Client",
    default_service = "org.freedesktop.GeoClue2"
)]
trait GeoClueClient {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_desktop_id(&self, id: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_requested_accuracy_level(&self, level: u32) -> zbus::Result<()>;

    #[zbus(property)]
    fn location(&self) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn location_updated(&self, old: OwnedObjectPath, new: OwnedObjectPath) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.GeoClue2.Location",
    default_service = "org.freedesktop.GeoClue2"
)]
trait GeoClueLocation {
    #[zbus(property)]
    fn latitude(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn longitude(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn accuracy(&self) -> zbus::Result<f64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known city coordinates resolve to the expected IANA zone. This also pins
    /// the (lng, lat) argument order — swapping it would land these in the wrong
    /// hemisphere and fail.
    #[test]
    fn lookup_known_cities() {
        let finder = DefaultFinder::new();
        let cases = [
            (43.6532, -79.3832, "America/Toronto"),   // Toronto
            (35.6762, 139.6503, "Asia/Tokyo"),        // Tokyo
            (-33.8688, 151.2093, "Australia/Sydney"), // Sydney
            (51.5074, -0.1278, "Europe/London"),      // London
        ];
        for (lat, lon, expected) in cases {
            assert_eq!(
                lookup_timezone(&finder, lat, lon).as_deref(),
                Ok(expected),
                "({lat}, {lon}) should resolve to {expected}"
            );
        }
    }

    /// The core auto-timezone fix: a precise Wi-Fi fix that arrives *after* the
    /// fast coarse IP fix must win, and only then is detection "good enough".
    /// (Values mirror the real bug: Charlotte IP fallback vs the true Wi-Fi fix.)
    #[test]
    fn best_fix_prefers_wifi_over_first_ip_fix() {
        let mut fix = BestFix::new();
        // Fast IP fix arrives first — recorded, but far too coarse to act on.
        fix.record(35.0562, -80.8194, 500_000.0); // Charlotte, IP fallback
        assert!(!fix.is_good_enough());
        assert_eq!(fix.coords(), Some((35.0562, -80.8194)));
        // Wi-Fi fix refines moments later — it wins, and we can stop waiting.
        fix.record(33.6178, -117.9277, 21.0); // Orange County, Wi-Fi
        assert!(fix.is_good_enough());
        assert_eq!(fix.coords(), Some((33.6178, -117.9277)));
    }

    #[test]
    fn best_fix_keeps_most_accurate_regardless_of_order() {
        let mut fix = BestFix::new();
        fix.record(33.6178, -117.9277, 21.0); // accurate first
        fix.record(35.0562, -80.8194, 500_000.0); // coarse later must NOT override
        assert_eq!(fix.coords(), Some((33.6178, -117.9277)));
    }

    #[test]
    fn best_fix_falls_back_to_coarse_when_nothing_better() {
        let mut fix = BestFix::new();
        // Only an IP-level fix ever arrives (Boston, ~25 km).
        fix.record(42.3601, -71.0589, 25_000.0);
        assert!(!fix.is_good_enough(), "25km must not satisfy the threshold");
        // It's still returned at timeout — a coarse guess beats no timezone.
        assert_eq!(fix.coords(), Some((42.3601, -71.0589)));
    }

    #[test]
    fn best_fix_empty_is_none() {
        let fix = BestFix::new();
        assert_eq!(fix.coords(), None);
        assert!(!fix.is_good_enough());
    }

    #[test]
    fn best_fix_threshold_boundary() {
        let mut at = BestFix::new();
        at.record(0.0, 0.0, GOOD_ACCURACY_M);
        assert!(
            at.is_good_enough(),
            "exactly at the threshold counts as good"
        );

        let mut above = BestFix::new();
        above.record(0.0, 0.0, GOOD_ACCURACY_M + 1.0);
        assert!(!above.is_good_enough(), "just above the threshold does not");
    }
}
