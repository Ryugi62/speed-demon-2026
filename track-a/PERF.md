# Track A: performance log

Local timings are x86-64 emulation (or arm64 native where stated) on a 2-CPU Colima VM and are directional only; the Run ID column stays empty until the board app is installed.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | Rust std only: 12-way newline-aligned split, pread, per-thread records, one counting sort, per-group sort_unstable | | 0.70-2.64 (2-CPU x86-64 emulation, includes container start; directional only) | 259 (cgroup peak 271.7 MB incl. page cache) | |

## Log

- Step 1 hypothesis: one pass plus one sort per group beats the reference (strptime is ~80% of its time). Result: output sha256 equals the published full-seed1 hash `495ca096...` in 7 of 9 amd64 Docker runs (3 unit tests green before the first run). 25 of 25 native arm64 runs also matched (about 0.5 s each).
- Open defect, not fixed: 2 of 9 amd64 runs under Rosetta never finished (main thread at 99% CPU, mostly system time, 1 thread, RSS flat at 49 MB, killed after about 10 minutes). It did not occur in 25 native arm64 runs, so it may be an emulation artefact, but this is unproven. Next experiment: fixed 12 threads instead of `available_parallelism`, with an in-container `timeout`.

- Step 2 hypothesis: the hang came from probing the host with `available_parallelism` under emulation; the board pins exactly 12 threads, so use a constant. A test asserting `THREADS == 12` failed to compile first (red), then passed after the change (4 unit tests green on amd64). Result: 46 amd64 Docker runs with an in-container `timeout 60` (board sandbox flags), 0 hangs and every finished run's output hash equals `495ca096...`; the first 30 of those included runs lost to a harness mistake of mine (the host-side output folder was deleted between runs, the program could not write `/out`), so the clean evidence is 30 of 30 runs, 0 hangs, 30 hash matches, 0.48-1.21 s each including container start (2-CPU emulation, directional only). Verdict: kept. Run ID column stays empty until the board returns one.
