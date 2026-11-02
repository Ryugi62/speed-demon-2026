# SPEC — Speed Demon 2026 entry (team BidGate)

## Purpose
Enter three tracks (A Ingest, C Join, D Traverse) of Speed Demon 2026 with programs whose output matches the reference byte for byte, and that beat the reference on the scoring formula
`K = (Tref/T)^0.50 x (Mref/M)^0.20 x (Sref/S)^0.15 x (Dref/D)^0.15`.

The headline technique is in track C: a speculative chunk-parallel CSV parser. Each chunk is parsed twice, once assuming it starts outside quotes and once assuming it starts inside, and the results are resolved left to right (after ParPaRaw, arXiv 1905.13415). A partition-invariance test guards it: the output hash must not depend on where the input is cut.

## Success criteria (numbers)
- S1: tracks A, C and D each produce an output whose sha256 equals the reference output's, on 5 of 5 local Docker runs (`check.py`).
- S2: zero runtime dependencies outside the language standard library (D term = 1).
- S3: track C passes the partition-invariance test on at least 1000 random cut points. Every cut point that ever broke the hash is kept in `track-c/tests/corpus/` and replayed.
- S4: every optimisation step is logged in that track's `PERF.md` with its hypothesis, measurement, verdict and, once the board app is installed, the board run ID. Reverted experiments stay in the log.

## Behaviour (Given / When / Then)
- Given the public dataset for a track in `/data` (read-only), when the container runs with `TRACK` set, then exactly one file is written to `/out` (`summary.txt`, `aggregates.txt` or `distances.txt`) and the process exits 0.
- Given any input split into N chunks at arbitrary byte offsets, when track C parses the chunks in parallel and resolves them, then the aggregates equal those of a single-threaded parse.
- Given an output that differs from the reference by one byte, when the test suite runs, then it fails before any speed change is accepted.

## Non-goals
- Track B (search index) is not entered; its folder is removed.
- No cached or precomputed results are shipped (organisers' rule: "Shipping cached results ... is disqualification").
- No dataframe, SQL, Arrow CSV, search-engine or shortest-path libraries (track bans).
- Local timings run under x86-64 emulation on an arm64 Mac. They are directional only; board run IDs are the measurements of record.
