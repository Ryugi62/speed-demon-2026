# SPEC - track A (Ingest), team BidGate

## Purpose
Read `/data/requests.log` (3.1M lines, `ts method endpoint status latency_us`, not sorted by time) and write `/out/summary.txt`: one line per (UTC hour, endpoint) group with count, nearest-rank p50/p95/p99, 5xx count and floor ppm rate. The output must equal the organisers' reference byte for byte. The contract is `specs/a-ingest.md` of the harness repository.

## Ubiquitous language
group = (hour, endpoint) pair; nearest rank = value at 1-indexed rank `(p*n+99)/100` of the sorted latencies; error = status 500..=599; error_ppm = `errors*1000000/count`, integer floor.

## Success criteria (numbers)
- S1: sha256 of `summary.txt` for seed 1 `full` equals `495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3` on 5 of 5 local Docker runs (`check.py`).
- S2: zero dependencies (Rust std only), `dependency_count = 0`.
- S3: the unit tests pass: the spec's worked example is reproduced byte for byte, n = 101 gives ranks 51/96/100, and the output does not depend on the thread count (1, 3, 12).
- S4: every optimisation step is logged in `PERF.md` (hypothesis, measurement, verdict, reverted experiments).

## Behaviour (Given / When / Then)
- Given the worked example of the spec, when the program runs, then `summary.txt` equals the ten expected lines.
- Given any input split among 1..12 threads at newline-aligned offsets, when the partial results are merged, then the output is identical to the single-threaded one.
- Given an output that differs by one byte from the reference, when `check.py --expected` runs, then it fails.

## Non-goals
- No cached or precomputed results; no dataframe or SQL engine; no dependencies; no handling of malformed lines (the spec promises well-formed input).
- Local timings run under emulation and are directional only.
