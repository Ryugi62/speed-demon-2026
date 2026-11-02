//! Track C (Join): speculative chunk-parallel CSV aggregation, standard library only.
//!
//! The input is cut into segments. Every worker guesses where the first record of
//! its segment starts and parses to the next segment's guess without knowing the
//! quote state at the cut. `aggregate` then resolves the segments left to right:
//! a segment is kept only if the previous one ended exactly at its start, else it
//! is parsed again from the proven record start. A wrong guess costs time, never
//! correctness.
use std::collections::HashMap;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
const HEADER: &[u8] = b"id,name,note,region_id,amount";
const NONE: u16 = u16::MAX;
const DENSE_MAX: u64 = 1 << 24;
const UNMATCHED: &[u8] = b"_UNMATCHED";

// ---------------------------------------------------------------- input access
pub trait ReadAt: Sync {
    fn len(&self) -> u64;
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<usize>;
}

impl ReadAt for std::fs::File {
    fn len(&self) -> u64 {
        self.metadata().map_or(0, |m| m.len())
    }
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<usize> {
        FileExt::read_at(self, buf, off)
    }
}

impl ReadAt for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<usize> {
        let off = (off as usize).min(<[u8]>::len(self));
        let n = buf.len().min(<[u8]>::len(self) - off);
        buf[..n].copy_from_slice(&self[off..off + n]);
        Ok(n)
    }
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub threads: usize,
    /// read window per worker in bytes; it grows when one record is bigger
    pub window: usize,
    /// explicit cut offsets (tests); `None` cuts evenly, one segment per thread
    pub cuts: Option<Vec<u64>>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { threads: 1, window: 1 << 20, cuts: None }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunInfo {
    pub segments: usize,
    pub redone: usize,
}

/// A byte window over a `ReadAt`, refilled on demand.
struct Win<'a, R: ReadAt + ?Sized> {
    src: &'a R,
    buf: Vec<u8>,
    base: u64,
    cap: usize,
}

impl<'a, R: ReadAt + ?Sized> Win<'a, R> {
    fn new(src: &'a R, cap: usize) -> Self {
        Win { src, buf: Vec::new(), base: 0, cap: cap.max(4) }
    }
    fn fill(&mut self, from: usize) -> io::Result<()> {
        self.buf.resize(self.cap, 0);
        let mut n = from;
        while n < self.cap {
            let got = self.src.read_at(&mut self.buf[n..], self.base + n as u64)?;
            if got == 0 {
                break;
            }
            n += got;
        }
        self.buf.truncate(n);
        Ok(())
    }
    fn load(&mut self, pos: u64) -> io::Result<()> {
        self.base = pos;
        self.fill(0)
    }
    /// Drop everything before `pos` and read more; the window doubles if it is full of one record.
    fn more(&mut self, pos: u64) -> io::Result<()> {
        let keep = (pos - self.base) as usize;
        self.buf.drain(..keep);
        self.base = pos;
        if self.buf.len() >= self.cap {
            self.cap *= 2;
        }
        let have = self.buf.len();
        self.fill(have)
    }
    fn at_eof(&self) -> bool {
        self.base + self.buf.len() as u64 >= self.src.len()
    }
}

// ------------------------------------------------------------------ byte search
#[cfg(target_arch = "x86_64")]
#[inline]
fn find(s: &[u8], from: usize, b: u8) -> Option<usize> {
    use std::arch::x86_64::*;
    let n = s.len();
    let mut i = from;
    // SSE2 is part of the x86-64 baseline, so these intrinsics are always available.
    unsafe {
        let pat = _mm_set1_epi8(b as i8);
        while i + 16 <= n {
            let v = _mm_loadu_si128(s.as_ptr().add(i) as *const __m128i);
            let m = _mm_movemask_epi8(_mm_cmpeq_epi8(v, pat)) as u32;
            if m != 0 {
                return Some(i + m.trailing_zeros() as usize);
            }
            i += 16;
        }
    }
    s[i.min(n)..].iter().position(|&x| x == b).map(|k| i + k)
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
fn find(s: &[u8], from: usize, b: u8) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let n = s.len();
    let mut i = from;
    let pat = LO.wrapping_mul(b as u64);
    while i + 8 <= n {
        let x = u64::from_le_bytes(s[i..i + 8].try_into().unwrap()) ^ pat;
        let z = x.wrapping_sub(LO) & !x & HI;
        if z != 0 {
            return Some(i + (z.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    s[i.min(n)..].iter().position(|&x| x == b).map(|k| i + k)
}

// ---------------------------------------------------------------- the lookup table
pub struct Regions {
    dense: Vec<u16>,
    sparse: HashMap<u64, u16>,
    keys: Vec<Vec<u8>>, // country codes, then _UNMATCHED last
}

impl Regions {
    pub fn parse(text: &[u8]) -> Result<Regions, String> {
        let mut lines = text.split(|&b| b == b'\n');
        let head = lines.next().unwrap_or(&[]);
        if head.strip_suffix(b"\r").unwrap_or(head) != b"region_id,country" {
            return Err("unexpected regions.csv header".into());
        }
        let mut code_ix: HashMap<Vec<u8>, u16> = HashMap::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut rows: Vec<(u64, u16)> = Vec::new();
        for line in lines {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            let c = line.iter().position(|&b| b == b',').ok_or("regions.csv: no comma")?;
            let id = parse_u64(&line[..c]).ok_or("regions.csv: bad region_id")?;
            let code = &line[c + 1..];
            let ix = match code_ix.get(code) {
                Some(&i) => i,
                None => {
                    let i = keys.len() as u16;
                    if keys.len() >= (NONE - 1) as usize {
                        return Err("too many countries".into());
                    }
                    keys.push(code.to_vec());
                    code_ix.insert(code.to_vec(), i);
                    i
                }
            };
            rows.push((id, ix));
        }
        keys.push(UNMATCHED.to_vec());
        let top = rows.iter().map(|r| r.0).filter(|&i| i < DENSE_MAX).max().map_or(0, |m| m + 1);
        let mut dense = vec![NONE; top as usize];
        let mut sparse = HashMap::new();
        for (id, ix) in rows {
            if id < DENSE_MAX {
                dense[id as usize] = ix;
            } else {
                sparse.insert(id, ix);
            }
        }
        Ok(Regions { dense, sparse, keys })
    }
    #[inline]
    fn lookup(&self, id: u64) -> usize {
        let un = self.keys.len() - 1;
        if (id as usize) < self.dense.len() && id < DENSE_MAX {
            let v = self.dense[id as usize];
            return if v == NONE { un } else { v as usize };
        }
        self.sparse.get(&id).map_or(un, |&v| v as usize)
    }
}

fn parse_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0u64;
    for &b in s {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add(d as u64);
    }
    Some(v)
}

// -------------------------------------------------------------------- aggregation
#[derive(Clone, Copy)]
struct Stat {
    count: u64,
    id_sum: u64,
    sum: i64,
    min: i64,
    max: i64,
    name_bytes: u64,
    note_bytes: u64,
}

const EMPTY: Stat = Stat { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 };

/// Interned name bytes: exact comparison, so "distinct" is exact too.
#[derive(Default)]
struct Names {
    slots: Vec<u32>, // name index + 1, 0 = empty
    hashes: Vec<u64>,
    spans: Vec<(u32, u32)>,
    arena: Vec<u8>,
}

#[inline]
fn hash(s: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = 0x9E37_79B9_7F4A_7C15u64 ^ s.len() as u64;
    let mut it = s.chunks_exact(8);
    for w in &mut it {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
    }
    let r = it.remainder();
    if !r.is_empty() {
        let mut t = [0u8; 8];
        t[..r.len()].copy_from_slice(r);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(t)).wrapping_mul(K);
    }
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^ (h >> 32)
}

impl Names {
    fn name(&self, i: u32) -> &[u8] {
        let (a, l) = self.spans[i as usize];
        &self.arena[a as usize..(a + l) as usize]
    }
    fn grow(&mut self) {
        let n = (self.slots.len() * 2).max(1024);
        self.slots = vec![0; n];
        for (i, &h) in self.hashes.iter().enumerate() {
            let mut p = h as usize & (n - 1);
            while self.slots[p] != 0 {
                p = (p + 1) & (n - 1);
            }
            self.slots[p] = i as u32 + 1;
        }
    }
    #[inline]
    fn intern(&mut self, s: &[u8]) -> u32 {
        if (self.hashes.len() + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let h = hash(s);
        let mask = self.slots.len() - 1;
        let mut p = h as usize & mask;
        loop {
            let v = self.slots[p];
            if v == 0 {
                break;
            }
            let i = v - 1;
            if self.hashes[i as usize] == h && self.name(i) == s {
                return i;
            }
            p = (p + 1) & mask;
        }
        let i = self.hashes.len() as u32;
        self.spans.push((self.arena.len() as u32, s.len() as u32));
        self.arena.extend_from_slice(s);
        self.hashes.push(h);
        self.slots[p] = i + 1;
        i
    }
}

struct Acc {
    st: Vec<Stat>,
    names: Names,
    seen: Vec<Vec<u64>>, // per country: bitset over local name indices
}

impl Acc {
    fn new(countries: usize) -> Acc {
        Acc { st: vec![EMPTY; countries], names: Names::default(), seen: vec![Vec::new(); countries] }
    }
    #[inline]
    fn add(&mut self, c: usize, r: &Rec, name: &[u8]) {
        let s = &mut self.st[c];
        s.count += 1;
        s.id_sum = s.id_sum.wrapping_add(r.id);
        s.sum = s.sum.wrapping_add(r.cents);
        s.min = s.min.min(r.cents);
        s.max = s.max.max(r.cents);
        s.name_bytes += r.name_len;
        s.note_bytes += r.note_len;
        let n = self.names.intern(name) as usize;
        let bits = &mut self.seen[c];
        if bits.len() <= n / 64 {
            bits.resize(n / 64 + 16, 0);
        }
        bits[n / 64] |= 1 << (n % 64);
    }
}

// ------------------------------------------------------------------ record parser
#[derive(Default)]
struct Rec {
    id: u64,
    name: (usize, usize),
    name_len: u64,
    note_len: u64,
    region: u64,
    cents: i64,
}

enum P {
    Done(usize),
    More,
    Bad,
}

/// One text field starting at `p`; `Err(P::More/Bad)` as in `parse`.
/// Returns (inner start, inner end, unescaped length, index after the comma).
#[inline]
fn text(s: &[u8], p: usize, eof: bool) -> Result<(usize, usize, u64, usize), P> {
    let n = s.len();
    let short = if eof { P::Bad } else { P::More };
    if p >= n {
        return Err(short);
    }
    if s[p] == b'"' {
        let a = p + 1;
        let mut q = a;
        let mut pairs = 0u64;
        loop {
            let i = match find(s, q, b'"') {
                Some(i) => i,
                None => return Err(short),
            };
            if i + 1 >= n {
                return Err(short);
            }
            if s[i + 1] == b'"' {
                pairs += 1;
                q = i + 2;
            } else if s[i + 1] == b',' {
                return Ok((a, i, (i - a) as u64 - pairs, i + 2));
            } else {
                return Err(P::Bad);
            }
        }
    }
    let mut q = p;
    loop {
        if q >= n {
            return Err(short);
        }
        match s[q] {
            b',' => return Ok((p, q, (q - p) as u64, q + 1)),
            b'"' | b'\r' | b'\n' => return Err(P::Bad),
            _ => q += 1,
        }
    }
}

/// Parse the record at the start of `s` (an optional BOM first).
#[inline]
fn parse(s: &[u8], eof: bool, r: &mut Rec) -> P {
    let n = s.len();
    let short = if eof { P::Bad } else { P::More };
    let mut p = 0;
    if n >= 3 {
        if s[..3] == BOM {
            p = 3;
        }
    } else if !eof && BOM.starts_with(s) {
        return P::More;
    }
    let st = p;
    let mut id = 0u64;
    loop {
        if p >= n {
            return short;
        }
        let b = s[p];
        if b == b',' {
            break;
        }
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return P::Bad;
        }
        id = id.wrapping_mul(10).wrapping_add(d as u64);
        p += 1;
    }
    if p == st {
        return P::Bad;
    }
    p += 1;
    let (ns, ne, nl, p2) = match text(s, p, eof) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (_, _, tl, p3) = match text(s, p2, eof) {
        Ok(v) => v,
        Err(e) => return e,
    };
    p = p3;
    let st = p;
    let mut region = 0u64;
    loop {
        if p >= n {
            return short;
        }
        let b = s[p];
        if b == b',' {
            break;
        }
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return P::Bad;
        }
        region = region.wrapping_mul(10).wrapping_add(d as u64);
        p += 1;
    }
    if p == st {
        return P::Bad;
    }
    p += 1;
    if p >= n {
        return short;
    }
    let neg = s[p] == b'-';
    p += neg as usize;
    let st = p;
    let mut whole = 0i64;
    loop {
        if p >= n {
            return short;
        }
        let b = s[p];
        if b == b'.' {
            break;
        }
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return P::Bad;
        }
        whole = whole.wrapping_mul(10).wrapping_add(d as i64);
        p += 1;
    }
    if p == st {
        return P::Bad;
    }
    if p + 3 > n {
        return short;
    }
    let (d1, d2) = (s[p + 1].wrapping_sub(b'0'), s[p + 2].wrapping_sub(b'0'));
    if d1 > 9 || d2 > 9 {
        return P::Bad;
    }
    let c = whole.wrapping_mul(100).wrapping_add((d1 * 10 + d2) as i64);
    p += 3;
    let end = if p >= n {
        if eof {
            p
        } else {
            return P::More;
        }
    } else if s[p] == b'\n' {
        p + 1
    } else if s[p] == b'\r' {
        if p + 1 >= n {
            return short;
        }
        if s[p + 1] != b'\n' {
            return P::Bad;
        }
        p + 2
    } else {
        return P::Bad;
    };
    r.id = id;
    r.name = (ns, ne);
    r.name_len = nl;
    r.note_len = tl;
    r.region = region;
    r.cents = if neg { c.wrapping_neg() } else { c };
    P::Done(end)
}

/// Header record: optional BOM, the fixed header line, a terminator. Returns its length.
fn header_len(s: &[u8], eof: bool) -> Result<usize, bool> {
    let p = if s.starts_with(&BOM) { 3 } else { 0 };
    let need = p + HEADER.len() + 2;
    if s.len() < need && !eof {
        return Err(true);
    }
    let rest = &s[p.min(s.len())..];
    if !rest.starts_with(HEADER) {
        return Err(false);
    }
    match &rest[HEADER.len()..] {
        [b'\n', ..] => Ok(p + HEADER.len() + 1),
        [b'\r', b'\n', ..] => Ok(p + HEADER.len() + 2),
        [] if eof => Ok(p + HEADER.len()),
        _ => Err(false),
    }
}

// ------------------------------------------------------------------- segment work
struct Seg {
    acc: Acc,
    end: u64,
}

/// Parse records that start in [start, limit); the last one may run past `limit`.
/// `Err(offset)` marks the record that did not parse (a wrong guess or bad input).
fn run_range<R: ReadAt + ?Sized>(win: &mut Win<R>, regions: &Regions, start: u64, limit: u64, header: bool) -> Result<Seg, u64> {
    let mut acc = Acc::new(regions.keys.len());
    let mut pos = start;
    if pos >= limit && !header {
        return Ok(Seg { acc, end: pos });
    }
    win.load(pos).map_err(|_| pos)?;
    if header {
        loop {
            match header_len(&win.buf, win.at_eof()) {
                Ok(n) => {
                    pos += n as u64;
                    break;
                }
                Err(true) => win.more(pos).map_err(|_| pos)?,
                Err(false) => return Err(pos),
            }
        }
    }
    let mut rec = Rec::default();
    while pos < limit {
        let off = (pos - win.base) as usize;
        let eof = win.at_eof();
        match parse(&win.buf[off..], eof, &mut rec) {
            P::Done(n) => {
                let name = &win.buf[off + rec.name.0..off + rec.name.1];
                acc.add(regions.lookup(rec.region), &rec, name);
                pos += n as u64;
            }
            P::More => win.more(pos).map_err(|_| pos)?,
            P::Bad => return Err(pos),
        }
    }
    Ok(Seg { acc, end: pos })
}

/// First plausible record start at or after `b`: an LF followed by (BOM) digits and a comma.
fn guess<R: ReadAt + ?Sized>(src: &R, b: u64) -> io::Result<u64> {
    let len = src.len();
    if b == 0 {
        return Ok(0);
    }
    let mut at = b - 1;
    let mut blk = [0u8; 4096];
    while at < len {
        let got = src.read_at(&mut blk, at)?;
        if got == 0 {
            break;
        }
        let mut i = 0;
        while let Some(k) = find(&blk[..got], i, b'\n') {
            let c = at + k as u64 + 1;
            if c >= len {
                return Ok(len);
            }
            let mut peek = [0u8; 48];
            let m = src.read_at(&mut peek, c)?;
            let q = if peek[..m].starts_with(&BOM) { 3 } else { 0 };
            let digits = peek[q..m].iter().take_while(|d| d.is_ascii_digit()).count();
            if digits > 0 && q + digits < m && peek[q + digits] == b',' {
                return Ok(c);
            }
            i = k + 1;
        }
        at += got as u64;
    }
    Ok(len)
}

fn bad_at(off: u64) -> String {
    format!("malformed record at byte {off}")
}

// ------------------------------------------------------------------------ driver
pub fn aggregate<R: ReadAt + ?Sized>(records: &R, regions: &[u8], opts: &Opts) -> Result<(Vec<u8>, RunInfo), String> {
    let regions = Regions::parse(regions)?;
    let len = records.len();
    let io = |e: io::Error| e.to_string();
    let threads = opts.threads.max(1);
    let mut cuts: Vec<u64> = match &opts.cuts {
        Some(c) => c.clone(),
        None => (1..threads as u64).map(|i| i * len / threads as u64).collect(),
    };
    cuts.retain(|&c| c > 0 && c < len);
    cuts.sort_unstable();
    cuts.dedup();
    let nseg = cuts.len() + 1;
    let mut g = vec![0u64; nseg + 1];
    for (i, &c) in cuts.iter().enumerate() {
        g[i + 1] = guess(records, c).map_err(io)?;
    }
    g[nseg] = len;

    // Speculative pass: every segment parses from its own guess, in parallel.
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Seg, u64>>>> = Mutex::new((0..nseg).map(|_| None).collect());
    std::thread::scope(|sc| {
        for _ in 0..threads.min(nseg) {
            sc.spawn(|| {
                let mut win = Win::new(records, opts.window);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= nseg {
                        break;
                    }
                    let r = run_range(&mut win, &regions, g[i], g[i + 1], i == 0);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    let results = results.into_inner().unwrap();

    // Resolve left to right: keep a segment only if it starts where the proven prefix ends.
    let mut win = Win::new(records, opts.window);
    let mut done: Vec<Acc> = Vec::with_capacity(nseg);
    let mut info = RunInfo { segments: nseg, redone: 0 };
    let mut pos = 0u64;
    for (i, r) in results.into_iter().enumerate() {
        match r.unwrap() {
            Ok(seg) if i == 0 || g[i] == pos => {
                pos = seg.end;
                done.push(seg.acc);
            }
            _ => {
                if i == 0 {
                    return Err(bad_at(0));
                }
                info.redone += 1;
                let seg = run_range(&mut win, &regions, pos, g[i + 1], false).map_err(bad_at)?;
                pos = seg.end;
                done.push(seg.acc);
            }
        }
    }
    if pos != len {
        return Err(bad_at(pos));
    }
    Ok((merge(&regions, done), info))
}

fn money(c: i64, out: &mut Vec<u8>) {
    if c < 0 {
        out.push(b'-');
    }
    let a = c.unsigned_abs();
    out.extend_from_slice(format!("{}.{:02}", a / 100, a % 100).as_bytes());
}

fn merge(regions: &Regions, accs: Vec<Acc>) -> Vec<u8> {
    let nc = regions.keys.len();
    let mut total = vec![EMPTY; nc];
    let mut global = Names::default();
    let mut gseen: Vec<Vec<u64>> = vec![Vec::new(); nc];
    for a in &accs {
        for (t, s) in total.iter_mut().zip(&a.st) {
            t.count += s.count;
            t.id_sum = t.id_sum.wrapping_add(s.id_sum);
            t.sum = t.sum.wrapping_add(s.sum);
            t.min = t.min.min(s.min);
            t.max = t.max.max(s.max);
            t.name_bytes += s.name_bytes;
            t.note_bytes += s.note_bytes;
        }
        let map: Vec<u32> = (0..a.names.hashes.len() as u32).map(|i| global.intern(a.names.name(i))).collect();
        for (c, bits) in a.seen.iter().enumerate() {
            for (w, &word) in bits.iter().enumerate() {
                let mut x = word;
                while x != 0 {
                    let g = map[w * 64 + x.trailing_zeros() as usize] as usize;
                    x &= x - 1;
                    let gb = &mut gseen[c];
                    if gb.len() <= g / 64 {
                        gb.resize(g / 64 + 16, 0);
                    }
                    gb[g / 64] |= 1 << (g % 64);
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..nc).filter(|&c| total[c].count > 0).collect();
    order.sort_by(|&a, &b| regions.keys[a].cmp(&regions.keys[b]));
    let mut out = Vec::new();
    for c in order {
        let t = &total[c];
        let distinct: u32 = gseen[c].iter().map(|w| w.count_ones()).sum();
        out.extend_from_slice(&regions.keys[c]);
        out.extend_from_slice(format!(" {} {} ", t.count, t.id_sum).as_bytes());
        money(t.sum, &mut out);
        out.push(b' ');
        money(t.min, &mut out);
        out.push(b' ');
        money(t.max, &mut out);
        out.extend_from_slice(format!(" {} {} {}\n", distinct, t.name_bytes, t.note_bytes).as_bytes());
    }
    out
}
