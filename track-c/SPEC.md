# SPEC — Track C (Join), team BidGate

## Purpose
Read `/data/records.csv` (hostile CSV, about 202 MB, 3.32 M records) and `/data/regions.csv`, join every record to its country, and write `/out/aggregates.txt` so that its sha256 equals the reference's, as fast and in as little memory as possible. The format is fixed by the organisers' track spec (`specs/c-join.md`): one line per group, `country count id_sum amount_sum amount_min amount_max names_distinct name_bytes note_bytes`, sorted bytewise, money in integer cents, LF after every line.

Headline technique: a **speculative, chunk-parallel parser** (after ParPaRaw, arXiv 1905.13415). The file is cut into segments at arbitrary byte offsets. Each worker guesses where the first record of its segment starts (the first LF that is followed by something shaped like a record start), parses from there without knowing whether the cut fell inside a quoted field, and stops at the next segment's guess. Results are then resolved left to right: a segment is accepted only if the previous segment's parse ended exactly where this one started; otherwise it is re-parsed from the proven record start. Correctness never depends on the guess, only speed does.

## Success criteria (numbers)
- C1: output sha256 on the seed-1 full dataset equals `d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536` on 5 of 5 local Docker runs (`check.py c --expected`).
- C2: **partition invariance** — for any set of cut points the output bytes equal the single-segment output. Tested on at least 1000 distinct cut points (exhaustive single cut at every byte of the fixtures plus random multi-cut trials), with read-window sizes down to 7 bytes. Every cut set that ever changed the output is stored in `tests/corpus/` and replayed on every test run.
- C3: the optimised parser equals an independent naive oracle (straight from the grammar, in the test code) on at least 200 random hostile files, and the Python reference on the worked example and on the full dataset.
- C4: zero dependencies (`Cargo.toml` has no `[dependencies]`), Rust standard library only.
- C5: peak RSS and wall time are logged for each step in `PERF.md`; local timings are directional only (x86-64 emulation on an arm64 Mac).

## Behaviour (Given / When / Then)
- Given the worked example of the organisers' spec (BOM, CRLF and LF mixed, quoted comma, `""`, embedded LF, unknown regions), when the program runs, then the output equals the four lines of the spec byte for byte.
- Given any input and any list of cut points, when the segments are parsed in parallel and resolved, then the output equals the single-segment output.
- Given a cut that falls inside a quoted field whose text contains an LF followed by digits and a comma (a fake record start), when the speculation picks that fake start, then the resolve step detects the mismatch, re-parses, and the output is unchanged.
- Given an output that differs from the reference by one byte, when the test suite runs, then it fails before any speed change is accepted.
- Given a record longer than the read window, when it is parsed, then the window grows and the output is unchanged.

## Non-goals
- No cached or precomputed results; the program reads `/data` on every run.
- No dataframe, SQL, Arrow, query-engine library, and no crates at all.
- Input that violates the grammar (the generator never writes it) is rejected with exit code 1, not repaired.
- No `unsafe` outside the byte-search primitive.
