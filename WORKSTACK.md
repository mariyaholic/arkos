# WORKSTACK

## Threads
- **Arkos H1 roadmap** (PR mariyaholic/arkos#2, draft, branch `claude/upbeat-dijkstra-u4yyk4`)
  - From→To: claude.review → owner (Maria)
  - Done: 21 roadmap items, review fixes, state machines recorded (`docs/state-machines.md`) and 14 approved defects fixed; CI green on Linux and Windows
  - Next: elevated Windows run of `tests/mft_windows.rs` and `scripts/measure.ps1`; decide on the walk and DB-size misses (`docs/measurements.md`); decide the deferred items in `docs/state-machines.md` (SM-4, S-10 heartbeat)
  - Files: `docs/roadmap-h1.md`, `docs/architecture-h1.md`, `docs/state-machines.md`, `src/`, `tests/`, `.github/workflows/`
  - Verify: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (Rust 1.100, also `--target x86_64-pc-windows-gnu`), `cargo test --all-targets`, `cargo test --no-default-features --all-targets`
  - Blockers: MFT reader never executed (needs elevated NTFS); walk 41k files/s on Linux vs 150k gate; DB 245 B/file vs 75 B gate needs a schema change

## Verification
- (runbook pending)
