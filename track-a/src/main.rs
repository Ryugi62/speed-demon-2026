//! Track A: per (UTC hour, endpoint) request count, nearest-rank percentiles and 5xx rate.
//! The log is cut at newline-aligned offsets, every thread parses its slice into compact
//! records, and one merge counting-sorts the records into groups so each group is sorted once.
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

const BLOCK: usize = 8 << 20;
const ERROR_BIT: u32 = 1 << 31;

/// One parsed line: hour number, endpoint id (+ error bit), latency.
type Record = [u32; 3];

#[derive(Default)]
struct Part {
    endpoints: HashMap<Vec<u8>, u32>,
    records: Vec<Record>,
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn digits(b: &[u8]) -> i64 {
    b.iter().fold(0, |n, &c| n * 10 + (c - b'0') as i64)
}

impl Part {
    /// Parses whole lines (each ending in LF) from `buf`.
    fn parse(&mut self, buf: &[u8]) {
        for line in buf.split(|&c| c == b'\n').filter(|l| !l.is_empty()) {
            let day = days_from_civil(digits(&line[0..4]), digits(&line[5..7]), digits(&line[8..10]));
            let hour = (day * 24 + digits(&line[11..13])) as u32;
            let rest = &line[28..];
            let ep_start = rest.iter().position(|&c| c == b' ').unwrap() + 1;
            let ep_len = rest[ep_start..].iter().position(|&c| c == b' ').unwrap();
            let endpoint = &rest[ep_start..ep_start + ep_len];
            let tail = &rest[ep_start + ep_len + 1..];
            let status = digits(&tail[..3]);
            let latency = digits(&tail[4..]) as u32;
            let next = self.endpoints.len() as u32;
            let id = *self.endpoints.entry(endpoint.to_vec()).or_insert(next);
            self.records.push([hour, id | if status >= 500 { ERROR_BIT } else { 0 }, latency]);
        }
    }

    /// Parses the file range `[start, end)`, which begins and ends on line boundaries.
    fn read_range(file: &File, start: u64, end: u64) -> Part {
        let mut part = Part::default();
        let mut buf = vec![0u8; BLOCK + 4096];
        let (mut pos, mut kept) = (start, 0);
        while pos < end {
            let want = (BLOCK as u64).min(end - pos) as usize;
            file.read_exact_at(&mut buf[kept..kept + want], pos).unwrap();
            pos += want as u64;
            let filled = kept + want;
            let cut = buf[..filled].iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
            part.parse(&buf[..cut]);
            buf.copy_within(cut..filled, 0);
            kept = filled - cut;
        }
        part
    }
}

/// First line boundary at or after `target`.
fn line_start(file: &File, size: u64, target: u64) -> u64 {
    let (mut pos, mut chunk) = (target.saturating_sub(1), [0u8; 512]);
    if target == 0 {
        return 0;
    }
    while pos < size {
        let n = ((size - pos) as usize).min(chunk.len());
        file.read_exact_at(&mut chunk[..n], pos).unwrap();
        if let Some(i) = chunk[..n].iter().position(|&c| c == b'\n') {
            return pos + i as u64 + 1;
        }
        pos += n as u64;
    }
    size
}

fn write_number(out: &mut Vec<u8>, mut n: u64) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&tmp[i..]);
}

fn run(data: &Path, out: &Path, threads: usize) {
    let file = File::open(data.join("requests.log")).unwrap();
    let size = file.metadata().unwrap().len();
    let cuts: Vec<u64> = (0..=threads as u64).map(|i| line_start(&file, size, size * i / threads as u64)).collect();
    let parts: Vec<Part> = std::thread::scope(|s| {
        let jobs: Vec<_> = cuts.windows(2).map(|w| s.spawn(|| Part::read_range(&file, w[0], w[1]))).collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    });

    // Global endpoint ranks in byte order, and the hour range.
    let mut names: Vec<&Vec<u8>> = parts.iter().flat_map(|p| p.endpoints.keys()).collect();
    names.sort_unstable();
    names.dedup();
    let rank: HashMap<&Vec<u8>, u32> = names.iter().enumerate().map(|(i, n)| (*n, i as u32)).collect();
    let hours = parts.iter().flat_map(|p| p.records.iter().map(|r| r[0]));
    let (lo, hi) = hours.fold((u32::MAX, 0), |(lo, hi), h| (lo.min(h), hi.max(h)));
    let total: usize = parts.iter().map(|p| p.records.len()).sum();
    let mut text = Vec::new();
    if total > 0 {
        let width = names.len();
        let groups = (hi - lo + 1) as usize * width;
        let remap: Vec<Vec<u32>> = parts.iter().map(|p| {
            let mut table = vec![0; p.endpoints.len()];
            for (name, &id) in &p.endpoints {
                table[id as usize] = rank[name];
            }
            table
        }).collect();
        let group_of = |part: usize, r: &Record| (r[0] - lo) as usize * width + remap[part][(r[1] & !ERROR_BIT) as usize] as usize;

        // Counting sort of latencies by group, then sort each group once.
        let mut start = vec![0usize; groups + 1];
        let mut errors = vec![0u32; groups];
        for (i, p) in parts.iter().enumerate() {
            for r in &p.records {
                let g = group_of(i, r);
                start[g + 1] += 1;
                errors[g] += (r[1] >> 31) as u32;
            }
        }
        for g in 0..groups {
            start[g + 1] += start[g];
        }
        let mut fill = start.clone();
        let mut latencies = vec![0u32; total];
        for (i, p) in parts.iter().enumerate() {
            for r in &p.records {
                let g = group_of(i, r);
                latencies[fill[g]] = r[2];
                fill[g] += 1;
            }
        }
        for g in 0..groups {
            let (a, b) = (start[g], start[g + 1]);
            if a == b {
                continue;
            }
            let lat = &mut latencies[a..b];
            lat.sort_unstable();
            let n = b - a;
            let (y, m, d) = civil_from_days((lo as i64 + (g / width) as i64).div_euclid(24));
            let hh = (lo as usize + g / width) % 24;
            text.extend_from_slice(format!("{y:04}-{m:02}-{d:02}T{hh:02}:00:00Z ").as_bytes());
            text.extend_from_slice(names[g % width]);
            let pick = |p: usize| lat[(p * n + 99) / 100 - 1] as u64;
            let fields = [n as u64, pick(50), pick(95), pick(99), errors[g] as u64, errors[g] as u64 * 1_000_000 / n as u64];
            for f in fields {
                text.push(b' ');
                write_number(&mut text, f);
            }
            text.push(b'\n');
        }
    }
    std::fs::write(out.join("summary.txt"), text).unwrap();
}

fn main() {
    let threads = std::thread::available_parallelism().map_or(12, |n| n.get());
    run(Path::new("/data"), Path::new("/out"), threads);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const EXAMPLE: &str = "2026-10-30T19:00:00.000000Z GET /v2/orders/{id} 200 1500
2026-10-30T18:59:59.999999Z GET /v2/orders/{id} 200 1841
2026-10-30T18:00:00.000000Z POST /v2/checkout 503 204117
2026-10-30T18:00:00.000000Z POST /v2/checkout 201 30000
2026-10-30T18:00:00.115332Z GET /health 200 62
2026-10-30T18:00:00.115332Z GET /health 200 62
2026-10-30T18:42:17.000001Z HEAD /health 200 4294967295
2026-10-30T18:30:00.000001Z OPTIONS / 200 0
2026-10-30T18:12:00.500000Z POST /v2/checkout 500 0
2026-10-30T18:12:00.600000Z POST /v2/checkout 499 1500
2026-10-30T18:12:00.700000Z POST /v2/checkout 599 2500
2026-10-30T18:20:00.000000Z GET /v2/orders/{id} 500 900
2026-10-30T18:21:00.000000Z DELETE /v2/orders/{id} 404 77
2026-10-30T19:05:00.000000Z GET /v2/orders_legacy 200 60
2026-10-30T19:05:00.000001Z POST /v2/orders-export 202 20
2026-10-30T19:05:00.000002Z GET /v2/orders 200 10
2026-10-31T23:59:59.999999Z POST /v2/checkout 502 7
2026-11-01T00:00:00.000000Z POST /v2/checkout 503 8
";
    const WANT: &str = "2026-10-30T18:00:00Z / 1 0 0 0 0 0
2026-10-30T18:00:00Z /health 3 62 4294967295 4294967295 0 0
2026-10-30T18:00:00Z /v2/checkout 5 2500 204117 204117 3 600000
2026-10-30T18:00:00Z /v2/orders/{id} 3 900 1841 1841 1 333333
2026-10-30T19:00:00Z /v2/orders 1 10 10 10 0 0
2026-10-30T19:00:00Z /v2/orders-export 1 20 20 20 0 0
2026-10-30T19:00:00Z /v2/orders/{id} 1 1500 1500 1500 0 0
2026-10-30T19:00:00Z /v2/orders_legacy 1 60 60 60 0 0
2026-10-31T23:00:00Z /v2/checkout 1 7 7 7 1 1000000
2026-11-01T00:00:00Z /v2/checkout 1 8 8 8 1 1000000
";

    fn summarize(name: &str, log: &str, threads: usize) -> String {
        let dir = std::env::temp_dir().join(format!("sd-a-{}-{name}-{threads}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("requests.log"), log).unwrap();
        run(&dir, &dir, threads);
        let text = fs::read_to_string(dir.join("summary.txt")).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        text
    }

    #[test]
    fn worked_example_matches_spec_for_any_thread_count() {
        for threads in [1, 3, 12, 40] {
            assert_eq!(summarize("ex", EXAMPLE, threads), WANT, "threads={threads}");
        }
    }

    #[test]
    fn nearest_rank_with_101_values() {
        let log: String = (0..101).rev().map(|i| format!("2026-10-30T18:00:00.000000Z GET /x 200 {}\n", i + 1)).collect();
        assert_eq!(summarize("p101", &log, 2), "2026-10-30T18:00:00Z /x 101 51 96 100 0 0\n");
    }

    #[test]
    fn empty_log_gives_empty_summary() {
        assert_eq!(summarize("empty", "", 4), "");
    }
}
