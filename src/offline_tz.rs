//! Offline IP → timezone fallback for automatic timezone.
//!
//! When GeoClue can't produce a fix — no usable Wi-Fi access points, or the
//! configured Wi-Fi backend errored without its own IP fallback (e.g. Google's
//! Geolocation API) — this makes a best-effort guess from the device's public IP:
//!   1. Learn the public IPv4 from an HTTPS IP-echo endpoint (only our own
//!      address is revealed — no Wi-Fi list, no third-party geolocation service).
//!   2. Look it up in a compact, embedded IP→timezone table built from DB-IP's
//!      "IP to City Lite" data (see `tools/gen_ip_timezones.py`).
//!
//! This is intentionally coarse (IP-level) — it exists so a moved device still
//! lands in a plausible zone when precise positioning is unavailable, not to
//! replace GeoClue. Returns `None` if the public IP can't be learned or the table
//! has no entry (including builds where the table wasn't generated).

use std::net::Ipv4Addr;
use std::sync::OnceLock;
use std::time::Duration;

/// Compact IP→timezone table (see module docs / `tools/gen-ip-timezones/`).
/// May be a header-only stub (zero ranges) when the data wasn't generated for
/// this build, in which case lookups simply return `None`.
static TABLE_BYTES: &[u8] = include_bytes!("../data/ip-timezones.bin");

/// How long to wait for each public-IP echo request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(6);

/// HTTPS endpoints that echo the caller's public IP. 443 is rarely blocked (even
/// behind captive NAT), and we reveal only our own address — no location.
const IP_ECHO_URLS: &[&str] = &[
    "https://1.1.1.1/cdn-cgi/trace",
    "https://api.ipify.org",
    "https://ifconfig.me/ip",
];

/// Detect the timezone from the device's public IP. `None` if unavailable.
pub async fn detect() -> Option<String> {
    let ip = public_ipv4().await?;
    let tz = table().lookup(ip)?;
    log::debug!("offline auto-timezone: {ip} -> {tz}");
    Some(tz.to_owned())
}

// ─── Embedded IP → timezone table ────────────────────────────────────────────

struct Table {
    names: Vec<String>,
    /// `(ip_start, tz_index)`, sorted ascending by `ip_start`. A range covers
    /// `[ip_start[i], ip_start[i + 1] - 1]`; `tz_index` 0 means "no data".
    ranges: Vec<(u32, u16)>,
}

impl Table {
    fn lookup(&self, ip: Ipv4Addr) -> Option<&str> {
        let key = u32::from(ip);
        // Largest range whose start <= key.
        let i = match self.ranges.binary_search_by(|(start, _)| start.cmp(&key)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let idx = self.ranges.get(i)?.1 as usize;
        match self.names.get(idx) {
            Some(name) if !name.is_empty() => Some(name.as_str()),
            _ => None,
        }
    }
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        parse_table(TABLE_BYTES).unwrap_or_else(|| {
            log::debug!("offline auto-timezone: embedded IP table missing/invalid");
            Table {
                names: Vec::new(),
                ranges: Vec::new(),
            }
        })
    })
}

fn parse_table(b: &[u8]) -> Option<Table> {
    // magic(4) version(1) reserved(1) tz_count(2)
    if b.len() < 8 || &b[0..4] != b"IPTZ" {
        return None;
    }
    let mut p = 6usize;
    let tz_count = u16::from_le_bytes([b[p], b[p + 1]]) as usize;
    p += 2;

    let mut names = Vec::with_capacity(tz_count);
    for _ in 0..tz_count {
        let len = *b.get(p)? as usize;
        p += 1;
        let s = b.get(p..p + len)?;
        names.push(String::from_utf8_lossy(s).into_owned());
        p += len;
    }

    let range_count = u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?) as usize;
    p += 4;
    let mut ranges = Vec::with_capacity(range_count);
    for _ in 0..range_count {
        let chunk = b.get(p..p + 6)?;
        let start = u32::from_le_bytes(chunk[0..4].try_into().ok()?);
        let idx = u16::from_le_bytes(chunk[4..6].try_into().ok()?);
        ranges.push((start, idx));
        p += 6;
    }
    Some(Table { names, ranges })
}

// ─── Public-IP discovery (HTTPS) ─────────────────────────────────────────────

async fn public_ipv4() -> Option<Ipv4Addr> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .ok()?;
    for url in IP_ECHO_URLS {
        if let Some(ip) = fetch_ip(&client, url).await {
            return Some(ip);
        }
    }
    log::debug!("offline auto-timezone: could not learn public IP");
    None
}

async fn fetch_ip(client: &reqwest::Client, url: &str) -> Option<Ipv4Addr> {
    let body = client.get(url).send().await.ok()?.text().await.ok()?;
    parse_ip_body(&body)
}

fn parse_ip_body(body: &str) -> Option<Ipv4Addr> {
    // Cloudflare's trace is "key=value" lines — take the `ip=` line specifically
    // (other keys like `h=1.1.1.1` would otherwise parse as a bogus address).
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("ip=")
            && let Ok(ip) = rest.trim().parse()
        {
            return Some(ip);
        }
    }
    // Bare-address endpoints return just the IP.
    body.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ip_body_handles_trace_and_bare() {
        // Cloudflare trace: must pick `ip=`, not the earlier `h=1.1.1.1`.
        let trace = "fl=1f\nh=1.1.1.1\nip=203.0.113.7\nts=1.2\n";
        assert_eq!(parse_ip_body(trace), Some(Ipv4Addr::new(203, 0, 113, 7)));
        // Bare-address endpoints.
        assert_eq!(
            parse_ip_body("198.51.100.9\n"),
            Some(Ipv4Addr::new(198, 51, 100, 9))
        );
        assert_eq!(parse_ip_body("not an ip"), None);
    }

    #[test]
    fn table_lookup_finds_range() {
        let t = Table {
            names: vec![
                String::new(),
                "America/Los_Angeles".into(),
                "America/New_York".into(),
            ],
            ranges: vec![(0, 0), (100, 1), (200, 2), (300, 0)],
        };
        assert_eq!(
            t.lookup(Ipv4Addr::from(150u32)),
            Some("America/Los_Angeles")
        );
        assert_eq!(t.lookup(Ipv4Addr::from(250u32)), Some("America/New_York"));
        assert_eq!(t.lookup(Ipv4Addr::from(50u32)), None); // index 0 = unknown
        assert_eq!(t.lookup(Ipv4Addr::from(350u32)), None); // gap
    }

    #[test]
    fn empty_table_returns_none() {
        let t = Table {
            names: Vec::new(),
            ranges: Vec::new(),
        };
        assert_eq!(t.lookup(Ipv4Addr::new(8, 8, 8, 8)), None);
    }

    /// The committed table embeds real DB-IP-derived data. Guard against a
    /// regenerated-but-empty table or a generator/parser format drift.
    #[test]
    fn embedded_table_is_populated() {
        let t = parse_table(TABLE_BYTES).expect("embedded IP table parses");
        assert!(
            t.names.len() > 100,
            "expected many timezones, got {}",
            t.names.len()
        );
        assert!(
            t.ranges.len() > 10_000,
            "expected many ranges, got {}",
            t.ranges.len()
        );
    }

    /// A routable public address resolves to a syntactically-valid IANA zone, and
    /// reserved space below the first allocated range resolves to nothing.
    #[test]
    fn embedded_table_resolves_public_and_reserved() {
        let t = parse_table(TABLE_BYTES).expect("embedded IP table parses");
        let tz = t
            .lookup(Ipv4Addr::new(8, 8, 8, 8))
            .expect("8.8.8.8 should resolve to a zone");
        assert!(
            tz.contains('/'),
            "expected an IANA zone like Area/City, got {tz:?}"
        );
        assert_eq!(t.lookup(Ipv4Addr::new(0, 0, 0, 0)), None);
    }

    /// End-to-end offline detection against the live network: learns the public
    /// IP over HTTPS, then maps it through the embedded table. Ignored by default
    /// (needs network); run manually:
    ///   cargo test --bin cosmic-settings-daemon \
    ///     offline_tz::tests::live_offline_detect -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "hits the live network to learn the public IP"]
    async fn live_offline_detect() {
        match detect().await {
            Some(tz) => println!("offline detected timezone: {tz}"),
            None => panic!("offline detection returned nothing"),
        }
    }
}
