# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md). Our own contract, with numeric success criteria, is [SPEC.md](SPEC.md); every optimisation step is in [PERF.md](PERF.md).

## Approach

Rust, standard library only. `records.csv` is cut into one segment per thread at arbitrary byte offsets. Each worker guesses where the first record of its segment starts (the first LF followed by an optional BOM, digits and a comma), parses from there without knowing whether the cut fell inside a quoted field, and stops at the next segment's guess. The results are then resolved left to right: a segment is kept only when the segment before it ended exactly where it began, otherwise it is parsed again from the proven record start. A wrong guess therefore costs time and never changes the output. Idea after ParPaRaw (arXiv 1905.13415).

Inside a segment: a record parser over a pread window (no mmap, no whole-file buffer), exact interning of names for the distinct count, a dense region table, integer cents and no floating point. The output is the only thing written.

Tests (`cargo test`): the organisers' worked example; every single cut at every byte of the fixtures with read windows down to 7 bytes; 300 random multi-cut trials; 220 random hostile files against a naive oracle written straight from the grammar; and `tests/corpus/`, the replayed cut sets. A cut set that ever changes the output goes into the corpus.

## Dependencies

None. `Cargo.toml` has no `[dependencies]`.

## With another 24 hours

Profile the record parser, try an AVX2 byte search behind runtime detection, share one name table across threads to cut memory, and measure the two-assumption variant (parse each chunk both ways) against the guess-and-verify one.
