# WORKSTACK

## Threads
- **Arkos H1 roadmap** (PR mariyaholic/arkos#2, draft, branch `claude/upbeat-dijkstra-u4yyk4`)
  - From→To: claude.review → owner (Maria)
  - Done: 21 roadmap items, review fixes, state machines recorded (`docs/state-machines.md`) and 14 approved defects fixed; merged in PR mariyaholic/arkos#2
  - Next: elevated Windows run of `tests/mft_windows.rs` and `scripts/measure.ps1`; decide on the walk and DB-size misses (`docs/measurements.md`); decide the deferred items in `docs/state-machines.md` (SM-4, S-10 heartbeat)
  - Files: `docs/roadmap-h1.md`, `docs/architecture-h1.md`, `docs/state-machines.md`, `src/`, `tests/`, `.github/workflows/`
  - Verify: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (Rust 1.100, also `--target x86_64-pc-windows-gnu`), `cargo test --all-targets`, `cargo test --no-default-features --all-targets`
  - Blockers: MFT reader never executed (needs elevated NTFS); walk 41k files/s on Linux vs 150k gate; DB 245 B/file vs 75 B gate needs a schema change

- **Arkos H1.1** (same branch)
  - From→To: claude.impl → claude.review (Opus)
  - Done: Q1 writer tuning (walk 35k to 80k files/s on Linux), Q2 lock identity check, Q3 heartbeat sidecar, Q4 `--experimental` MFT gate plus `scripts/compare-mft.ps1` and prerelease rule, Q5 placeholders and long-path test, Q6 `docs/schema-0002-plan.md`, deferred items updated
  - Next: review; Windows hand test of the MFT reader on a USB stick or VHD; owner decision on the 0002 migration number (DupliCut holds 0002 in spec section 4.2)
  - Files: `docs/h1-1.md`, `docs/schema-0002-plan.md`, `docs/measurements.md`, `docs/state-machines.md`, `src/lock.rs`, `src/scan/`, `src/db/mod.rs`, `scripts/`
  - Verify: the four checks above plus the Windows clippy target
  - Blockers: walk still under the 150k files/s floor; Windows-only code (identity check, placeholders, long-path test, compare-mft.ps1) is type-checked and unrun

- **Arkos security review** (PR mariyaholic/arkos#3, draft, same branch)
  - From→To: claude.review → owner (Maria)
  - Done: SQLite 3.54.1 (RUSTSEC-2026-0459), workflow secrets scoped and actions pinned, CI token read-only, compare-mft PATH fallback removed, atomic `--out`, bidi controls shown, elevated-run guard (`docs/elevated-writes.md`, option D, refuse)
  - Next: owner marks the PR ready and merges; elevated hand test of the guard and the MFT reader on a USB stick or VHD
  - Files: `src/elevated.rs`, `src/db/mod.rs`, `src/tree.rs`, `src/lock.rs`, `.github/workflows/`, `scripts/compare-mft.ps1`, `tests/elevated_*.rs`, `tests/support/mod.rs`
  - Verify: the checks above plus `cargo audit`; CI green on Linux and Windows at `2175ccc`
  - Blockers: none for merge; `a_pinned_folder_cannot_be_renamed_until_released` and the elevated scan test need real elevated NTFS

## Verification
- 2026-11-18, Windows build under Wine 11 (MinGW, `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine`): 91 of 92 tests pass, including `state_machines.rs` and `long_paths_windows.rs`. `case_only_rename_shows_as_one_removed_and_one_added` fails because Wine on a case-sensitive host does not apply a case-only rename (`ren Foo foo` leaves `Foo`); real NTFS is expected to pass. Wine has no NTFS volume, so `mft_windows.rs` stays unrun.
- 2026-11-19, GitHub Actions on PR mariyaholic/arkos#3 at `2175ccc`: `check (ubuntu-latest)` and `check (windows-latest)` green. First green Windows CI; the case-only rename test passes on real NTFS. Two runner-only failures came from the `RUNNER~1` short temp path and were fixed in the tests (`754d784`, `2175ccc`).
