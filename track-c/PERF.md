# Track C: performance log

Every step from the reference to the final version. The run-ID column is empty until the board app is installed on this repo; every number below is a **local** measurement and is directional only. Reverted experiments stay in the log.

Local setup for steps 1-2: Colima VM, **arm64 native** (not the x86-64 emulation `check.py` uses), 2 vCPU shared with other containers, data copied into a tmpfs, `time` around the process alone. The board's reference figures (14.44 s, 367 MB) are from the template, not from this machine.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged (Python csv module) | | 14.44 | 367 | 0 |
| 1 | Rust, std only: one segment, single thread, 1 MiB read window | | 2.9-3.5 (local, 3 runs) | 23 (VmHWM) | |
| 2 | Speculative segments, 2 / 4 / 12 threads | | 1.6-4.0 / 1.7-2.3 / 2.0-2.4 (local, 3 runs each, very noisy) | 33 / 53 / 132 | |

## Ledger

### Step 0: correctness first (before any speed)
- Hypothesis: the organisers' published hash for seed-1 `full` is right and the reference reproduces it.
- Measured: reference run in `python:3.12-slim` (not the allowlisted 3.13 image; same `csv` semantics) on the verified dataset (records.csv and regions.csv match `SHA256SUMS.txt`): `aggregates.txt` sha256 `d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`, equal to the spec's published value. Local time 10.7 s in the VM.
- Verdict: the expected hash is confirmed independently, so the hash test is the acceptance gate for every later step.

### Step 1: single-threaded Rust, tests first
- Order: the 8 tests in `tests/partition.rs` were written against a `todo!()` stub and failed 8 of 8; then the implementation. The worked example of the organisers' spec is one of them.
- Design: records are parsed in a read window (`pread`, 1 MiB, grows if one record is bigger); names are interned into an exact byte table (no hash-only distinct count); `distinct` is a per-country bitset over interned names; the region lookup is a dense `u16` table.
- Measured: sha256 equals the expected hash on the first real run, with 1, 2, 4 and 12 threads (3 runs each, all identical). Single-thread time 2.9-3.5 s, VmHWM 23 MB.
- Verdict: correct. Speed is far below what the data allows (about 65 MB/s); the parse loop has not been profiled yet. Next hypothesis: the time is the per-record work in `parse` (bounds checks, three passes over the digits), not I/O.

### Step 2: speculative parallel segments
- Hypothesis: cutting the file anywhere and guessing each segment's first record start (first LF followed by an optional BOM, digits and a comma) lets the segments run with no pre-pass; a left-to-right check (previous segment ended exactly where this one started) catches every wrong guess.
- Deviation from the first plan, stated plainly: the plan was "parse every chunk twice, once assuming inside quotes and once outside". The implementation guesses one start per segment and verifies it by the end-position chain, so a wrong guess costs one re-parse of that segment, and no work is doubled on the normal path. The two-assumption variant was not built, so there is no measurement of it.
- Test result (arm64, opt-level 2): 8 of 8 pass; exhaustive single cut at every byte of 14 hostile files with read windows of 7, 19 and 256 bytes; 300 random multi-cut trials; 220 files against the naive oracle; the corpus. At least 1000 distinct cut points are asserted in `at_least_a_thousand_cut_points_were_exercised`. No cut ever changed the output, so `tests/corpus/` holds hand-made fake-record-start cases (named `fakestart-*`), not hash breakers, and each is replayed with 3 window sizes.
- Mutation check: with the chain check removed (`Ok(seg)` accepted unconditionally), `replay_corpus` and `random_multi_cut_trials` fail, so the tests do catch a broken resolve step. The first run after restoring the file still used the mutated binary (the restore kept the old file time, so cargo did not rebuild); `touch` fixed it. Do not trust a green run right after a `mv` restore.
- Measured on the real file: `redone=0` on every run, so the guess was right at every cut on this data.
- Verdict: correct; the local speed-up is not credible yet (2 shared vCPUs, noise larger than the effect). Memory grows with threads (23 MB at 1, 132 MB at 12) because each thread owns a name table and bitsets; that is the next thing to shrink.

## 계획 대비 바뀐 점
- 계획: 청크마다 「따옴표 안 / 밖」 두 가정으로 둘 다 파싱한 뒤 왼쪽부터 확정.
- 구현: 구간마다 레코드 시작을 한 번 추정하고, 앞 구간이 끝난 위치와 같은지 연쇄로 검증하며, 어긋나면 그 구간만 재파싱.
- 이유: 정상 경로에서 작업이 2배가 되지 않는다. 정확성은 추정이 아니라 검증이 지킨다(시험으로 확인).
- 두 가정 방식은 만들지 않았고 측정값도 없다.

### Step 3: allowlisted linux/amd64 image, `check.py` 5 of 5
- Image: `rust:1-slim-trixie` (pinned digest) build, `debian:13-slim` runtime, image 30 MB. Data = seed-1 full, files verified against `SHA256SUMS.txt`.
- Measured: sha256 `d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536` on all 5 runs (PASS). Time printed by `check.py` including container start-up: 2.60, 2.68, 2.71, 3.46, 2.55 s (emulated x86-64 on 2 shared vCPUs; directional only, board run IDs are the record).
- Verdict: S1 met locally. Still open: `cargo test` inside amd64 (first execution of the SSE2 search path by the tests), profiling, memory.

## Not yet done (state at the stop)
- SSE2 search path (`x86_64`) has only been compiled for arm64 so far; its first execution is in the amd64 container.
- Profiling and the throughput work; shrinking per-thread memory; lines of code and `/usr/bin/time -v` table (VmHWM from `SD_REPORT=1` is used instead of `time -v`).
