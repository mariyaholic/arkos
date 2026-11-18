# Arkos roadmap to H1

Source: three advisory passes (2026-11-17) checked against the code on `main` at `191e4ed`. Each
item records what the code already does, so work starts from the gap and not from the suggestion.
The design for every item is in [`architecture-h1.md`](architecture-h1.md).

## Decisions taken (owner: Maria, 2026-11-17)

| Question | Decision |
|---|---|
| Allocated size | Logical size only. Reports say allocated size may be smaller. Schema 0001 stays frozen. |
| Hardlinks | Spec §5.3 holds: counted per name, and every report prints the caveat. |
| Archive extensions (open question 3) | A fixed list of compound extensions (`tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz`, `tar.lzma`, `tar.z`) is stored whole; every other name keeps the last dot. |
| Subtree retention (open question 1) | Retention keeps the newest N per `(volume, root)`, so subtree scans never push out a full-volume snapshot. |
| DupliCut schema drift | No schema change. A golden test pins the 0001 column list, and a written read contract tells consumers what to check. |

## Items

| # | Item | State in code before this work | Phase |
|---|---|---|---|
| 1 | Snapshot `mode` records the path that ran | MFT stub stamps `mft` while walking | P1 |
| 2 | `diff --top N` is one ranked list | N applies to dirs and files separately | P1 |
| 3 | Human sizes in the terminal, raw bytes in `--json` | Terminal prints raw `7988054368 B` | P1 |
| 4 | Platform path separator in file paths | File paths join with `\` on Unix | P1 |
| 5 | `prune --dry-run` | `prune` without `--yes` only errors | P1 |
| 6 | Scan progress on stderr | No output until the scan ends | P1 |
| 7 | Interrupted scan becomes `abandoned` on next open | Only a 24 h stale sweeper | P2 |
| 8 | Fixture generator and budget measurement | `examples/gen_tree.rs` and `scripts/measure.ps1` from spec §1 do not exist | P5 |
| 9 | Golden test: diff names the expected top five directories | No such test | P2 |
| 10 | Nested squarified treemap; drop the near-red category colour | Single row of top-level slices; Documents uses `--wine` | P4 |
| 11 | Hardlink caveat in reports (decision above) | Caveat absent | P3 |
| 12 | Exclude the Arkos data directory when it sits inside the scan root | Not excluded | P2 |
| 13 | A database error mid-scan leaves a `failed` snapshot with no partial rows | Snapshot marked `failed`, partial rows kept | P2 |
| 14 | Edge cases: non-Unicode names, case-only renames, FAT32/exFAT under `auto` | Untested | P2 |
| 15 | Real MFT reader (`FSCTL_ENUM_USN_DATA`, two phases) | Stub that walks | P6 |
| 16 | Allocated-size note in reports (decision above) | Note absent | P3 |
| 17 | Compound archive extensions (decision above) | Last dot only | P3 |
| 18 | Retention per `(volume, root)` (decision above) | One cap per volume | P3 |
| 19 | Schema golden test and consumer read contract | Arkos guards `user_version` on write only | P7 |
| 20 | User guide with the exit-code table | README quickstart only | P7 |
| 21 | CI and a Windows release build with SHA-256 checksum | No workflows | P7 |

Verified as already done before this work: the volume lock and its test (Unix fix in PR
mariyaholic/arkos#2), the exit-4 refusal for unelevated `--mode mft`, the `denied`/`skipped`
counters and the HTML warning, the `user_version` write guard, and reparse points recorded at size 0
and never followed.

Out of H1: the egui panel and CSV export (H2), scheduled snapshots (H3), hashing (DupliCut) and
cleaning (Haki).
