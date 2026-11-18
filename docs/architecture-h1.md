# Arkos H1 baseline architecture

This document fixes the design for every item in [`roadmap-h1.md`](roadmap-h1.md). The build
contract in [`spec-m0.md`](spec-m0.md) still governs; where this document narrows it, the section is
cited. Item numbers match the roadmap.

## Invariants every phase keeps

- Schema 0001 is frozen. No new tables, columns, indexes or views in this work.
- No new runtime dependencies. Dev-dependencies are allowed when a test needs one.
- No network code, no scripts in the HTML report, every path in HTML passes through `html_escape`.
- Gold is the one accent. `--lining` (and `--wine`, which reads as red) is for warnings only.
- Exit codes stay on the shared 0 to 6 table in `src/exit.rs`.
- `--json` output keeps raw integers; human formatting applies to text output only.
- Every phase ends green on: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets`, `cargo test --no-default-features --all-targets`, and
  `cargo check --target x86_64-pc-windows-gnu --all-targets`.

## Module map after H1

| Module | Change |
|---|---|
| `src/fmt.rs` (new) | `format_size` moves here from `tree.rs`; `join_display(dir, name)` builds file paths |
| `src/progress.rs` (new) | Throttled stderr progress reporter |
| `src/scan/mod.rs` | Mode is resolved after the scanner reports what ran; data-dir exclusion; failure cleanup |
| `src/scan/walk.rs` | Progress hook; non-Unicode name policy; uses `fmt::join_display` |
| `src/scan/mft.rs` | Real two-phase USN enumeration on Windows volume roots |
| `src/scan/paths.rs` | Compound extension list |
| `src/db/mod.rs` | `open_and_migrate` runs the lock-aware abandoned sweep |
| `src/db/retention.rs` | Prune partitions by `(volume, root)`; dry-run plan |
| `src/diff.rs` | One ranked list across dirs and files |
| `src/tree.rs` | Squarified nested layout; caveat notes; colour set without red |
| `examples/gen_tree.rs` (new) | Deterministic fixture generator with manifest hash |
| `scripts/measure.ps1`, `scripts/measure.sh` (new) | Budget measurement runners |
| `tests/` | New contract tests listed per phase |
| `docs/user-guide.md`, `docs/schema-contract.md`, `docs/measurements.md` (new) | Docs |
| `.github/workflows/ci.yml`, `release.yml` (new) | CI and release |

## P1: CLI correctness

**1. Mode records what ran.** Scanners return the mode they actually used. Change the scanner
result to `ScanOutcome { stats: WalkStats, mode: &'static str }`. `mft_into_snapshot` returns
`"mft"` only when the USN path ran; on a subtree root (which USN enumeration cannot scope) it walks
and returns `"walk"`. `run_scan` inserts the row with the requested mode as today, then updates
`mode` together with `state = 'complete'`. The `mode` CHECK constraint (`mft`, `walk`) is
unchanged; `mft-slow` stays a log note (spec §5.2).

**2. Diff top N.** `diff_snapshots` builds one list of `Change { kind: Dir|File, change, delta,
path }`, sorts by `abs(delta)` descending then path ascending, and truncates to N. `--min-size`
filters before truncation. The JSON shape keeps `dirs` and `files` arrays for consumers, each
holding only the entries that survived the shared cut.

**3. Human sizes.** `fmt::format_size(bytes: i64) -> String` (binary units, one decimal, `B` under
1 KiB) is used by `scan`, `list`, `top`, `diff`, `status` text output and the HTML. Negative deltas
print with a sign (`-1.4 GiB`). Columns are right-aligned to a fixed width so `diff` output lines up.

**4. Path separator.** `fmt::join_display(dir: &str, name: &str) -> String` joins with
`std::path::MAIN_SEPARATOR`, and with no separator when `dir` already ends in one. Every place that
concatenates a stored dir path and a file name uses it. Stored values do not change: `arkos_dirs.path`
and `arkos_files.name` are written exactly as before.

**5. Prune dry run.** `retention::plan_prune(conn, volume, keep) -> Vec<PruneCandidate>` returns
what would go. `prune_completed` calls the same planner, so the preview and the deletion cannot
disagree. CLI: `--dry-run` prints the plan and exits 0; `--yes` deletes; neither flag prints the plan
and exits 2 with the hint to pass `--yes`.

**6. Progress.** `progress::Reporter` prints `scanned N files, M dirs, X` to stderr at most every
500 ms, overwriting the line with `\r`, and clears it at the end. It is active only when stderr is a
terminal (`std::io::IsTerminal`) and `--json` is not set. The walk calls `reporter.tick(files, dirs,
bytes)`; the reporter decides whether to print, so the walk pays one `Instant` comparison per batch.

## P2: Integrity

**7. Interrupted scans.** A process that dies mid-scan releases its volume lock (the OS drops
`flock` and `LockFileEx` locks with the handle). `retention::sweep_abandoned` therefore marks a
`running` snapshot `abandoned` when its volume lock can be taken without waiting, and keeps the
24-hour rule as a fallback for a lock directory that cannot be read. It takes the lock with a zero
timeout and releases it before returning. `run_scan` holds the lock before inserting its `running`
row, so a live scan is never swept.

**9. Diff golden test.** `tests/diff_golden.rs` builds a fixture with the generator's library code
(see P5), takes snapshot A, applies a scripted mutation set that grows, shrinks, adds and deletes in
known directories, takes snapshot B, and asserts the first five directory entries of the diff by path
and order.

**12. Own data directory.** When the scan root contains `Paths::home`, the walk excludes that
directory by exact path (not by glob), and the scan log records `excluded_data_dir`. This keeps the
ledger, its WAL and the log out of the snapshot.

**13. Database failure mid-scan.** When the scanner returns an error after the `running` row is
inserted, `run_scan` deletes that snapshot's `arkos_files` and `arkos_dirs` rows in one transaction
and marks the row `failed` with `finished_at`. If that cleanup itself fails (for example the disk is
full), the row stays `running` and the lock-aware sweep turns it `abandoned` on the next open. `diff`,
`top` and `tree` already refuse anything that is not `complete`.

**14. Edge cases.**
- Non-Unicode names (unpaired UTF-16 surrogates on Windows, non-UTF-8 bytes on Unix): the entry is
  not stored, `skipped` is incremented, and the log line carries the lossy form. Storing a lossy name
  could collide with a real name under the `(snapshot_id, dir_id, name)` key.
- Case-only renames: diff compares stored paths byte for byte, so `Foo` to `foo` shows as one
  `Removed` and one `Added`. The user guide says so.
- FAT32/exFAT/ReFS under `--mode auto`: `is_ntfs` is false, so auto picks the walk; `--mode mft`
  exits 5 (`Unsupported`). A unit test covers the resolution table with injected predicates.

## P3: Reporting policy

**11 and 16. Caveats.** The HTML report and `scan` text output carry one note line: sizes are logical
and per name; hardlinked files may be counted more than once, and allocated size may be smaller for
compressed, sparse or cloud placeholder files. JSON gets `"size_basis": "logical_per_name"`.

**17. Compound extensions.** `normalize_ext` checks a fixed lowercase suffix list (`tar.gz`,
`tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz`, `tar.lzma`, `tar.z`) before falling back to the last dot.
The name must be longer than the suffix plus its dot, so a file named `.tar.gz` stays a dotfile.

**18. Retention per root.** `plan_prune` partitions completed snapshots by `(volume, root)` and keeps
the newest N in each partition. `running` snapshots are never candidates; `failed` and `abandoned`
rows are always candidates, whatever the number of complete snapshots in their partition.

## P4: Treemap

**10.** `tree.rs` builds a nested treemap with the squarified algorithm (Bruls, Huizing, van Wijk)
over `arkos_dirs` down to `--depth`, in one SVG with a fixed `viewBox` that scales to the page.

- Each directory's rectangle is inset by a header strip for its label, and its children (sub-folders,
  plus one "files here" block for direct file bytes) are laid out inside the remaining area.
- Rectangles below 4 px by 4 px in viewBox units are not emitted; their bytes stay in the parent.
- Colour comes from the top-level ancestor, cycling through `--gold`, `--lavender`, `--sage`,
  `--marigold`, `--gold-dim`, `--platinum`. Deeper levels keep the hue and step down opacity.
- Each rectangle has an SVG `<title>` with the escaped path and size, which browsers show on hover
  with no script.
- The folder and file tables below the map stay as they are.
- Budget: `arkos tree` for 100k dirs under 4 s and under 40 MB output (spec §6). Rendering is linear
  in emitted rectangles, and the size floor bounds their count.

## P5: Measurement

**8.** `examples/gen_tree.rs --files N --dirs M --depth D --seed S --out PATH` writes a
deterministic tree (sizes via `File::set_len`, so it is sparse and fast) and prints a manifest hash
over the sorted `(path, size)` list. The generator's logic lives in `tests/support/fixture.rs` so
tests reuse it without a library dependency on test code.

- `tests/budgets.rs` holds `#[ignore]` tests: `walk_throughput_on_100k` asserts at least 150,000
  files/s in release mode, and `diff_100k_under_2s`.
- `scripts/measure.sh` (Unix) and `scripts/measure.ps1` (Windows) build release, generate the
  fixture, run scan, diff and tree three times each, and print the median for every row of spec §6
  they can measure on that platform. Peak memory uses `/usr/bin/time -v` on Linux and
  `PeakWorkingSetSize` on Windows.
- `docs/measurements.md` records each run with date, platform, fixture hash and numbers. Linux runs
  are labelled as not counting toward the Windows gates.

## P6: MFT reader

**15.** On `cfg(all(windows, feature = "mft"))`, and only when the scan root is a volume root:

1. Open `\\.\X:` with `GENERIC_READ`, share read and write.
2. Phase 1: `FSCTL_ENUM_USN_DATA` from `StartFileReferenceNumber = 0`, keeping directory records only
   as `FRN -> (parent FRN, name)` in a `HashMap`. Above 1,000,000 directories, log `mft-slow` and fall
   back to the walk for this scan (the walk is correct, only slower). The snapshot then records `walk`.
3. Resolve each directory's path by following parents to the root FRN, with memoisation; insert
   `arkos_dirs` rows in depth order through the same batching writer the walk uses.
4. Phase 2: enumerate again and, for each file record, look up its parent's `dir_id`. A missing
   parent increments `denied` (spec §5.2). Size comes from `GetFileInformationByHandleEx` only for
   files the USN record cannot size, per spec §5.2 `--detail-over`; mtime from the USN timestamp.
5. Reparse points and excluded names follow the same policy as the walk.

The code compiles in this container through `cargo check --target x86_64-pc-windows-gnu`, but it
cannot run here. A test gated on `cfg(windows)` and `#[ignore]` (`walk_matches_mft_on_the_same_tree`)
waits for an elevated Windows run, and the PR must say that this path is unexecuted.

## P7: Contract, docs, release

**19.** `tests/schema_contract.rs` reads `PRAGMA table_info` for the three `arkos_*` tables and
compares it to a checked-in list, so any column change fails loudly. `docs/schema-contract.md` tells
consumers to refuse a ledger whose `user_version` is below 1, to read only `state = 'complete'`
snapshots, and to join by `(snapshot_id, path)` per spec §4.2.

**20.** `docs/user-guide.md`: install, the verbs with one example each, the exit-code table,
why the walk is the default, what `denied` means, case-only renames, and the size caveats.

**21.** `.github/workflows/ci.yml` runs the phase checks on `ubuntu-latest` and `windows-latest`.
`release.yml` runs on a `v*` tag, builds `x86_64-pc-windows-msvc` in release, writes
`arkos.exe.sha256`, and attaches both to the release. Code signing is a separate step that runs only
when the repository has a signing certificate secret configured; no secret values live in the
workflow file.
