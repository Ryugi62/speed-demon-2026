//! Partition invariance, oracle equality, the spec's worked example and the corpus replay.
mod common;
use common::*;
use sdjoin::{aggregate, Opts};
use std::sync::atomic::{AtomicUsize, Ordering};

static CUTS_TESTED: AtomicUsize = AtomicUsize::new(0);

const REGIONS_EX: &[u8] = b"region_id,country\n3,BBB\n7,AAA\n12,AAA\n20,CCC\n21,DDD\n";
const EXPECT_EX: &str = "AAA 5 17 1517.75 -0.75 1420.00 4 24 55\nBBB 1 4 -5.25 -5.25 -5.25 1 15 35\nCCC 1 9 -0.05 -0.05 -0.05 1 3 1\n_UNMATCHED 2 15 1.00 -1.00 2.00 2 21 1\n";

fn worked_records() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"\xEF\xBB\xBFid,name,note,region_id,amount\r\n");
    v.extend_from_slice(b"1,\"Kaphar, Titus\",\"said \"\"no\"\" twice\",7,1420.00\r\n");
    v.extend_from_slice(b"2,\"Betts\",\"a note that\nspans two lines\",7,88.50\n");
    v.extend_from_slice(b"3,,\"\",12,0.00\r\n");
    v.extend_from_slice(b"4,\"trailing space \",\"comma, quote \"\" and newline\ntogether\",3,-5.25\n");
    v.extend_from_slice(b"\xEF\xBB\xBF5,\"Betts \",\"one\ntwo\nthree\",12,-0.75\r\n");
    v.extend_from_slice(b"6,\"\",,12,10.00\r\n");
    v.extend_from_slice(b"\xEF\xBB\xBF7,\"Ann \"\"Annie\"\" Lee\",\"x\",5,2.00\n");
    v.extend_from_slice(b"8,\"Nobody\",\"\",4000000,-1.00\r\n");
    v.extend_from_slice(b"9,\"Neg\",\"n\",20,-0.05\n");
    v
}

fn serial(rec: &[u8], reg: &[u8], window: usize) -> Vec<u8> {
    let o = Opts { threads: 1, window, cuts: Some(vec![]) };
    aggregate(rec, reg, &o).unwrap().0
}

#[test]
fn worked_example_matches_the_spec() {
    let rec = worked_records();
    let out = serial(&rec, REGIONS_EX, 1 << 20);
    assert_eq!(String::from_utf8(out).unwrap(), EXPECT_EX);
    assert_eq!(oracle(&rec, REGIONS_EX), EXPECT_EX.as_bytes());
}

#[test]
fn worked_example_every_single_cut_and_small_windows() {
    let rec = worked_records();
    for window in [7usize, 16, 64, 4096] {
        for cut in 0..=rec.len() as u64 {
            for threads in [1usize, 3] {
                let o = Opts { threads, window, cuts: Some(vec![cut]) };
                let (out, _) = aggregate(&rec[..], REGIONS_EX, &o).unwrap();
                assert_eq!(String::from_utf8(out).unwrap(), EXPECT_EX, "cut {} window {}", cut, window);
                CUTS_TESTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[test]
fn engine_equals_oracle_on_random_hostile_files() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for t in 0..220 {
        let (nr, nn) = (5 + rng.below(40) as usize, 1 + rng.below(60) as usize);
        let reg = gen_regions(&mut rng, nr, t % 3 == 0);
        let rec = gen_records(&mut rng, &reg, nn, t % 2 == 0);
        let want = oracle(&rec, &reg);
        let got = serial(&rec, &reg, [7usize, 33, 1 << 16][t % 3]);
        assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "trial {}", t);
    }
}

#[test]
fn exhaustive_single_cut_on_hostile_files() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let mut redone = 0;
    for t in 0..14 {
        let reg = gen_regions(&mut rng, 12, t % 4 == 0);
        let rec = gen_records(&mut rng, &reg, 18, true);
        let want = oracle(&rec, &reg);
        for cut in 0..=rec.len() as u64 {
            let window = [7usize, 19, 256][(cut % 3) as usize];
            let o = Opts { threads: 2, window, cuts: Some(vec![cut]) };
            let (got, info) = aggregate(&rec[..], &reg, &o).unwrap();
            assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "file {} cut {} window {}", t, cut, window);
            redone += info.redone;
            CUTS_TESTED.fetch_add(1, Ordering::Relaxed);
        }
    }
    assert!(redone > 0, "the adversarial files never forced a re-parse, so the fallback path is untested");
}

#[test]
fn random_multi_cut_trials() {
    let mut rng = Rng(0x0bad_5eed_1234_4321);
    for t in 0..300 {
        let nn = 40 + rng.below(160) as usize;
        let reg = gen_regions(&mut rng, 30, t % 5 == 0);
        let rec = gen_records(&mut rng, &reg, nn, t % 2 == 1);
        let want = oracle(&rec, &reg);
        let k = 1 + rng.below(16) as usize;
        let mut cuts: Vec<u64> = (0..k).map(|_| rng.below(rec.len() as u64 + 1)).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let window = [7usize, 50, 1000, 1 << 20][rng.below(4) as usize];
        let o = Opts { threads: 1 + rng.below(5) as usize, window, cuts: Some(cuts.clone()) };
        let (got, _) = aggregate(&rec[..], &reg, &o).unwrap();
        assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "trial {} cuts {:?} window {}", t, cuts, window);
        CUTS_TESTED.fetch_add(cuts.len(), Ordering::Relaxed);
    }
}

#[test]
fn rejects_malformed_input() {
    let reg = REGIONS_EX;
    for bad in [
        &b"id,name,note,region_id,amount\n1,a,b,7,1.5\n"[..],         // amount needs two digits
        &b"id,name,note,region_id,amount\nx,a,b,7,1.50\n"[..],        // id not a number
        &b"id,name,note,region_id,amount\n1,\"a\"b,b,7,1.50\n"[..],   // closing quote not before a comma
        &b"id,name,note,region_id,amount\n1,a,b,7,1.50\r"[..],        // lone CR
        &b"id,name,wrong,region_id,amount\n1,a,b,7,1.50\n"[..],       // header
    ] {
        assert!(aggregate(bad, reg, &Opts::default()).is_err(), "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn replay_corpus() {
    // tests/corpus/<name>.csv + <name>.regions + <name>.cuts ("window cut,cut,...")
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut n = 0;
    let mut fake_redone = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().map_or(true, |x| x != "csv") {
            continue;
        }
        let rec = std::fs::read(&p).unwrap();
        let reg = std::fs::read(p.with_extension("regions")).unwrap();
        let want = oracle(&rec, &reg);
        for line in std::fs::read_to_string(p.with_extension("cuts")).unwrap().lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
            let (w, c) = line.split_once(' ').unwrap();
            let cuts: Vec<u64> = c.split(',').filter(|x| !x.is_empty()).map(|x| x.parse().unwrap()).collect();
            let o = Opts { threads: 3, window: w.parse().unwrap(), cuts: Some(cuts) };
            let (got, info) = aggregate(&rec[..], &reg, &o).unwrap_or_else(|e| panic!("{} {} -> {}", p.display(), line, e));
            assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "{} {}", p.display(), line);
            if p.file_name().unwrap().to_string_lossy().starts_with("fakestart") {
                fake_redone += info.redone;
            }
            n += 1;
        }
    }
    assert!(n >= 1, "the corpus is empty");
    assert!(fake_redone > 0, "the fakestart cases no longer force a re-parse; the corpus lost its point");
}

#[test]
fn at_least_a_thousand_cut_points_were_exercised() {
    // runs last by name only by convention; counts what the other tests in this binary have done so far
    let mut rng = Rng(77);
    let reg = gen_regions(&mut rng, 20, false);
    let rec = gen_records(&mut rng, &reg, 60, true);
    let want = oracle(&rec, &reg);
    let mut distinct = std::collections::BTreeSet::new();
    for cut in 0..=rec.len() as u64 {
        let o = Opts { threads: 2, window: 11, cuts: Some(vec![cut]) };
        assert_eq!(aggregate(&rec[..], &reg, &o).unwrap().0, want, "cut {}", cut);
        distinct.insert(cut);
    }
    assert!(distinct.len() >= 1000, "only {} distinct cut points", distinct.len());
}
