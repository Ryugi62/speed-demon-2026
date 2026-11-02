# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The log is cut into newline-aligned slices, one per thread; each thread parses lines into compact records. A single merge ranks endpoints by byte order, counting-sorts latencies into (hour, endpoint) groups, sorts each group once and reads the nearest-rank percentiles.

How your solution works, in plain prose.

## Dependencies

None.

## With another 24 hours

What you would do next.
