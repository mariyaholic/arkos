# WORKSTACK

## Threads
- **Arkos H1 roadmap, P1 to P7** (branch `claude/upbeat-dijkstra-u4yyk4`, local commits, not pushed)
  - From→To: claude.impl → claude.review
  - Done: all 21 roadmap items in seven commits; Windows-only MFT reader (`src/scan/mft.rs`, `usn.rs`) type-checks and has never run
  - Next: review the P6 deviations (per-file `OpenFileById` sizing, hardlinks counted once); run `tests/mft_windows.rs` and `scripts/measure.ps1` elevated on Windows; decide what to do about the walk and DB-size misses in `docs/measurements.md`
  - Files: `src/scan/`, `src/db/retention.rs`, `src/diff.rs`, `src/treemap.rs`, `src/fmt.rs`, `src/progress.rs`, `tests/`, `examples/gen_tree.rs`, `scripts/`, `docs/`, `.github/workflows/`
  - Verify: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo test --no-default-features --all-targets`, `cargo check --target x86_64-pc-windows-gnu --all-targets`
  - Blockers: no Windows host here; workflows and `measure.ps1` unrun; `walk_throughput_on_100k` fails on this Linux host (about 28k files/s)

## Verification
- (runbook pending)
