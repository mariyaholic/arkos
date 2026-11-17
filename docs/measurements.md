# Measurements

Each run names its date, platform, fixture and numbers. Gates are from spec section 6. Linux runs
are labelled and do not count toward the Windows gates; the 1M-file and MFT rows wait for a
Windows run of `scripts/measure.ps1`.

Method: `scripts/measure.sh` (Unix) or `scripts/measure.ps1` (Windows) builds release, generates
the fixture with `examples/gen_tree.rs`, runs scan three times (median), takes a second snapshot
after changing about one file in a hundred, then runs diff three times, tree three times
(`--depth 12`) and `list` ten times. Fixture sizes are sparse, so the byte totals are logical.

## 2026-11-17, Linux, 100k files

Platform: Linux 6.18 x86_64 in a 4-core container VM (not a Windows gate run). Build: release,
`opt-level = "s"`, LTO.

| Row | 100k files, 12k dirs | 100k files, 100k dirs | Gate (spec section 6) |
|---|---|---|---|
| Fixture hash | `7246fc3c7d94762b` | `06eac4a6e6256ae7` | |
| Walk files/s | 28,011 | 10,438 | at least 200,000 (fails under 150,000) |
| Rollup ms | 60 | 954 | under 3,000 at 1M files |
| Peak working set | 11.2 MB | 22.8 MB | at most 60 MB |
| DB bytes per snapshot | 24.5 MB | 65.4 MB | at most 75 MB per 1M files |
| Diff ms | 600 | 1,975 | under 2,000 at 1M files |
| Tree ms (`--depth 12`) | 52 | 256 | under 4,000 at 100k dirs |
| Tree output | 0.70 MB | 0.75 MB | under 40 MB |
| `list` cold ms | 28 | 27 | under 200 |
| Binary bytes | 2,065,368 | 2,065,368 | at most 8,000,000 |

Reading the numbers:

- Walk throughput is far below the gate on this machine, and it falls with directory count. Most of
  the wall time is spent writing rows to SQLite (`pwrite64` and WAL checkpoints every 256 pages),
  not reading the tree: `find -type f -printf` over the same 100k files takes 0.3 s. This host is
  not the Windows target, so the result is a pointer and not a verdict, but a Windows run should
  expect to look at the writer first.
- DB bytes per snapshot scale to about 245 bytes per file on the 12k-directory tree. At 1M files
  that is above the 75 MB gate. The second fixture has one directory per file, which inflates the
  directory rows; its 654 bytes per file is an upper bound for the shape, not a typical tree.
- Tree and `list` are well inside their gates. The tree output is small because the size floor
  drops rectangles under 4 by 4 units.
- The 1M-file rows (MFT, diff, rollup, DB bytes) are extrapolations in the gate column only; none
  of them has been measured at 1M files.

`cargo test --release -- --ignored` runs `walk_throughput_on_100k` and `diff_100k_under_2s`. On
this host the first fails by design of the gate (about 22,000 files/s against 150,000), and the
second passes.
