//! Regenerate `../../data/ip-timezones.bin`, the compact IP->timezone table that
//! `src/offline_tz.rs` embeds for the offline auto-timezone fallback.
//!
//! Input: DB-IP "IP to City Lite" IPv4 CSV (CC BY 4.0). The free CSV leaves the
//! timezone column empty but provides latitude/longitude, so we resolve the zone
//! with tzf-rs (the same boundary data the daemon uses) and then merge adjacent
//! IP ranges that share a timezone — collapsing the ~4M city rows down to the
//! timezone boundaries (tens of thousands of ranges).
//!
//! Download the source, then run:
//!   curl -L https://raw.githubusercontent.com/sapics/ip-location-db/main/dbip-city/dbip-city-ipv4.csv.gz -o dbip.csv.gz
//!   cargo run --release -- dbip.csv.gz ../../data/ip-timezones.bin
//!
//! Output format (little-endian):
//!   "IPTZ" | u8 version=1 | u8 reserved | u16 tz_count
//!   tz_count x (u8 len + UTF-8 name)        ; index 0 = "" = unknown
//!   u32 range_count
//!   range_count x (u32 ip_start, u16 tz_index)  ; sorted ascending by ip_start
//! A range covers [ip_start[i], ip_start[i+1]-1]; index 0 means "no data".

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::net::Ipv4Addr;

use flate2::read::GzDecoder;
use tzf_rs::DefaultFinder;

fn main() {
    let mut args = std::env::args().skip(1);
    let (inp, outp) = match (args.next(), args.next()) {
        (Some(i), Some(o)) => (i, o),
        _ => {
            eprintln!("usage: gen-ip-timezones <dbip-city-ipv4.csv[.gz]> <out.bin>");
            std::process::exit(2);
        }
    };

    let file = std::fs::File::open(&inp).expect("open input");
    let reader: Box<dyn Read> = if inp.ends_with(".gz") {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let reader = BufReader::new(reader);

    let finder = DefaultFinder::new();

    let mut names: Vec<String> = vec![String::new()]; // index 0 = unknown
    let mut name_index: HashMap<String, u16> = HashMap::from([(String::new(), 0)]);
    // Cache tz lookups by a ~0.05° grid cell so we call tzf-rs ~once per cell,
    // not once per row.
    let mut cell_cache: HashMap<(i32, i32), u16> = HashMap::new();
    let mut ranges: Vec<(u32, u16)> = Vec::new();
    let mut last_idx: Option<u16> = None;
    let mut prev_end: i64 = -1;
    let mut rows: u64 = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 9 {
            continue;
        }
        let (Ok(start_ip), Ok(end_ip)) =
            (cols[0].parse::<Ipv4Addr>(), cols[1].parse::<Ipv4Addr>())
        else {
            continue;
        };
        let start = u32::from(start_ip);
        let end = u32::from(end_ip);
        rows += 1;

        let lat: f64 = cols[7].parse().unwrap_or(f64::NAN);
        let lon: f64 = cols[8].parse().unwrap_or(f64::NAN);
        let idx = if lat.is_nan() || lon.is_nan() {
            0
        } else {
            let cell = ((lat * 20.0) as i32, (lon * 20.0) as i32);
            *cell_cache.entry(cell).or_insert_with(|| {
                let tz = finder.get_tz_name(lon, lat);
                if tz.is_empty() {
                    0
                } else if let Some(&i) = name_index.get(tz) {
                    i
                } else {
                    let i = names.len() as u16;
                    names.push(tz.to_string());
                    name_index.insert(tz.to_string(), i);
                    i
                }
            })
        };

        // Fill any gap before this row with "unknown" so a query landing in
        // unassigned space doesn't inherit the previous timezone.
        if prev_end >= 0 && i64::from(start) > prev_end + 1 && last_idx != Some(0) {
            ranges.push((prev_end as u32 + 1, 0));
            last_idx = Some(0);
        }
        if Some(idx) != last_idx {
            ranges.push((start, idx));
            last_idx = Some(idx);
        }
        prev_end = prev_end.max(i64::from(end));
    }

    let out = std::fs::File::create(&outp).expect("create output");
    let mut w = BufWriter::new(out);
    w.write_all(b"IPTZ").unwrap();
    w.write_all(&[1u8, 0u8]).unwrap();
    w.write_all(&(names.len() as u16).to_le_bytes()).unwrap();
    for name in &names {
        let b = name.as_bytes();
        w.write_all(&[b.len() as u8]).unwrap();
        w.write_all(b).unwrap();
    }
    w.write_all(&(ranges.len() as u32).to_le_bytes()).unwrap();
    for (start, idx) in &ranges {
        w.write_all(&start.to_le_bytes()).unwrap();
        w.write_all(&idx.to_le_bytes()).unwrap();
    }
    w.flush().unwrap();

    eprintln!(
        "rows={rows} timezones={} ranges={} -> {outp}",
        names.len(),
        ranges.len()
    );
}
