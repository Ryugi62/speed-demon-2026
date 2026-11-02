//! Shared test support: a tiny PRNG, a hostile-CSV generator, and a naive oracle
//! written straight from the grammar (it shares no code with the engine).
#![allow(dead_code)]
use std::collections::{BTreeMap, HashMap, HashSet};

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

pub const BOM: &[u8] = b"\xEF\xBB\xBF";
pub const HEADER: &[u8] = b"id,name,note,region_id,amount";

pub fn gen_regions(rng: &mut Rng, n: usize, with_huge: bool) -> Vec<u8> {
    let mut ids: Vec<u64> = Vec::new();
    let mut id = 0;
    for _ in 0..n {
        id += 1 + rng.below(9);
        ids.push(id);
    }
    if with_huge {
        ids.push(20_000_000 + rng.below(1000));
        ids.push(4_000_000_000 + rng.below(1000));
    }
    let letters = b"ABCDEFGHJKMNPQRSTUVWXYZ";
    let codes: Vec<String> = (0..(n / 3).max(2))
        .map(|_| (0..3).map(|_| *rng.pick(letters) as char).collect())
        .collect();
    let mut out = b"region_id,country\n".to_vec();
    for id in ids {
        out.extend(format!("{},{}\n", id, rng.pick(&codes)).as_bytes());
    }
    out
}

fn quoted(text: &[u8]) -> Vec<u8> {
    let mut v = vec![b'"'];
    for &b in text {
        v.push(b);
        if b == b'"' {
            v.push(b'"');
        }
    }
    v.push(b'"');
    v
}

/// Text for a name or note. `adversarial` plants fake record starts
/// (LF + digits + comma + quote) and BOM bytes inside quoted fields.
fn text(rng: &mut Rng, adversarial: bool) -> Vec<u8> {
    match rng.below(10) {
        0 => return Vec::new(),
        1 => return b"\"\"".to_vec(),
        2 => {
            // an unquoted non-empty field: legal in the grammar, never written by the generator
            return (0..1 + rng.below(5)).map(|_| *rng.pick(b"ab 9Z")).collect();
        }
        _ => {}
    }
    let pieces: [&[u8]; 15] = [
        b"abc", b"x", b" ", b",", b"\"", b"\n", b"\r\n", b"12,", b"\n7,\"", b"\n34,x,\"", BOM, "한é".as_bytes(), b"  ", b"\n", b"\n99,a,b,7,1.00\r\n",
    ];
    let mut t = Vec::new();
    for _ in 0..rng.below(if adversarial { 9 } else { 5 }) {
        let p = *rng.pick(&pieces);
        if !adversarial && (p.contains(&b'\n') && p.len() > 1) {
            continue;
        }
        t.extend_from_slice(p);
    }
    quoted(&t)
}

pub fn gen_records(rng: &mut Rng, regions: &[u8], n: usize, adversarial: bool) -> Vec<u8> {
    let ids: Vec<u64> = regions
        .split(|&b| b == b'\n')
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| std::str::from_utf8(l).unwrap().split(',').next().unwrap().parse().unwrap())
        .collect();
    let mut out = Vec::new();
    if rng.below(4) != 0 {
        out.extend_from_slice(BOM);
    }
    out.extend_from_slice(HEADER);
    out.extend_from_slice(if rng.below(2) == 0 { b"\r\n" } else { b"\n" });
    let mut id = 0u64;
    for i in 0..n {
        if rng.below(6) == 0 {
            out.extend_from_slice(BOM);
        }
        id += 1 + rng.below(3);
        let region = match rng.below(20) {
            0 => rng.below(300),
            1 => 10_000_000 + rng.below(100),
            2 => 20_000_000 + rng.below(1500),
            3 => 4_000_000_000 + rng.below(1500),
            _ => *rng.pick(&ids),
        };
        let cents = rng.below(2_000_000) as i64 - 400_000;
        let amount = if cents < 0 { format!("-{}.{:02}", -cents / 100, -cents % 100) } else { format!("{}.{:02}", cents / 100, cents % 100) };
        out.extend(format!("{},", id).as_bytes());
        out.extend(text(rng, adversarial));
        out.push(b',');
        out.extend(text(rng, adversarial));
        out.extend(format!(",{},{}", region, amount).as_bytes());
        if i + 1 < n || rng.below(3) != 0 {
            out.extend_from_slice(if rng.below(2) == 0 { b"\r\n" } else { b"\n" });
        }
    }
    out
}

pub fn money(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    let a = c.unsigned_abs();
    format!("{}{}.{:02}", sign, a / 100, a % 100)
}

/// Naive oracle: byte-by-byte grammar, owned Vec fields, HashSet of names.
pub fn oracle(rec: &[u8], regions: &[u8]) -> Vec<u8> {
    let mut map: HashMap<u64, Vec<u8>> = HashMap::new();
    for l in regions.split(|&b| b == b'\n').skip(1).filter(|l| !l.is_empty()) {
        let s = std::str::from_utf8(l).unwrap();
        let (a, b) = s.split_once(',').unwrap();
        map.insert(a.parse().unwrap(), b.as_bytes().to_vec());
    }
    struct G {
        n: i64,
        ids: i64,
        sum: i64,
        min: i64,
        max: i64,
        names: HashSet<Vec<u8>>,
        nb: i64,
        tb: i64,
    }
    let mut groups: BTreeMap<Vec<u8>, G> = BTreeMap::new();
    let mut i = 0;
    let mut first = true;
    while i < rec.len() {
        if rec[i..].starts_with(BOM) {
            i += 3;
        }
        let mut f: Vec<Vec<u8>> = Vec::new();
        loop {
            let mut v = Vec::new();
            if rec.get(i) == Some(&b'"') {
                i += 1;
                loop {
                    let c = rec[i];
                    if c == b'"' {
                        if rec.get(i + 1) == Some(&b'"') {
                            v.push(b'"');
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        v.push(c);
                        i += 1;
                    }
                }
            } else {
                while i < rec.len() && !matches!(rec[i], b',' | b'\r' | b'\n') {
                    v.push(rec[i]);
                    i += 1;
                }
            }
            f.push(v);
            if rec.get(i) == Some(&b',') {
                i += 1;
            } else {
                break;
            }
        }
        if rec.get(i) == Some(&b'\r') && rec.get(i + 1) == Some(&b'\n') {
            i += 2;
        } else if rec.get(i) == Some(&b'\n') {
            i += 1;
        } else {
            assert_eq!(i, rec.len(), "oracle: bad terminator at {}", i);
        }
        assert_eq!(f.len(), 5);
        if first {
            assert_eq!(f.join(&b','), HEADER);
            first = false;
            continue;
        }
        let id: i64 = std::str::from_utf8(&f[0]).unwrap().parse().unwrap();
        let region: u64 = std::str::from_utf8(&f[3]).unwrap().parse().unwrap();
        let a = std::str::from_utf8(&f[4]).unwrap();
        let (neg, a) = a.strip_prefix('-').map_or((false, a), |r| (true, r));
        let (w, fr) = a.split_once('.').unwrap();
        assert_eq!(fr.len(), 2);
        let cents = w.parse::<i64>().unwrap() * 100 + fr.parse::<i64>().unwrap();
        let cents = if neg { -cents } else { cents };
        let key = map.get(&region).cloned().unwrap_or_else(|| b"_UNMATCHED".to_vec());
        let g = groups.entry(key).or_insert(G { n: 0, ids: 0, sum: 0, min: i64::MAX, max: i64::MIN, names: HashSet::new(), nb: 0, tb: 0 });
        g.n += 1;
        g.ids += id;
        g.sum += cents;
        g.min = g.min.min(cents);
        g.max = g.max.max(cents);
        g.nb += f[1].len() as i64;
        g.tb += f[2].len() as i64;
        g.names.insert(f[1].clone());
    }
    let mut out = Vec::new();
    for (k, g) in groups {
        out.extend(k);
        out.extend(format!(" {} {} {} {} {} {} {} {}\n", g.n, g.ids, money(g.sum), money(g.min), money(g.max), g.names.len(), g.nb, g.tb).as_bytes());
    }
    out
}
