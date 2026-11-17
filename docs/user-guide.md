# Arkos user guide

Arkos scans a volume or folder into a local SQLite ledger, rolls folder sizes up, compares
snapshots and writes a static HTML treemap. It only reads your files and makes no network calls.

## Install

Build from source with `cargo build --release`, or take `arkos.exe` and `arkos.exe.sha256` from a
release. Check the download with `certutil -hashfile arkos.exe SHA256` and compare it to the
checksum file. Set `ARKOS_HOME` to keep the ledger somewhere else; run `arkos paths` to see where
it lives now.

## Commands

| Command | Example |
|---|---|
| `scan` | `arkos scan --volume C: --label before-update` |
| `list` | `arkos list --volume C: --limit 5` |
| `top` | `arkos top --snapshot 3 --dirs --limit 10` |
| `diff` | `arkos diff 2 3 --top 20 --min-size 1048576` |
| `tree` | `arkos tree --snapshot 3 --out report.html --depth 4` |
| `prune` | `arkos prune --keep 4 --dry-run` then `arkos prune --keep 4 --yes` |
| `status` | `arkos status --probe` |
| `db` | `arkos db info` |

Text output uses human sizes (`1.4 GiB`). Add `--json` where a command offers it for raw byte
counts. `scan` draws a progress line on stderr when stderr is a terminal. `--volume` takes a drive
letter or an existing folder, and `--path` limits a scan to a subfolder of the volume.

`diff --top N` returns one list of the N largest changes, folders and files together, ranked by
absolute change. `prune` keeps the newest N complete snapshots for each volume and root, so a
scan of one folder never pushes out a full-volume snapshot. Without `--yes` or `--dry-run` it shows
the plan and exits 2.

## Exit codes

| Code | Name | Meaning |
|---|---|---|
| 0 | ok | Done. |
| 1 | io | A file or database operation failed. |
| 2 | cli | Bad arguments, or `prune` without `--yes` or `--dry-run`. |
| 3 | input | A snapshot does not exist, is not complete, or the two snapshots are on different volumes. |
| 4 | refused | `--mode mft` without an elevated token, an existing output file, or another scan on the volume. The ledger is also refused when a newer Arkos wrote it. |
| 5 | unsupported | `--mode mft` on a build without the feature or on a volume that is not NTFS. |
| 6 | busy | The volume lock or the database stayed busy past the timeout. |

## Why the walk is the default

The walk works on every filesystem and needs no elevation. `--mode auto` uses the MFT reader only
for an elevated prompt on an NTFS volume root, and walks otherwise, including FAT32, exFAT and
ReFS. The snapshot records the path that actually ran. The MFT reader covers a whole volume, so a
scan of a subfolder always walks. If a volume has more than a million folders, or cannot be
opened, the scan walks and the log notes why.

`--mode mft` counts each hardlinked file once, because USN enumeration returns one name per
file, while the walk counts every name. Totals from the two modes can therefore differ on volumes
with many hardlinks, such as WinSxS. `--mode mft` also sizes each file with one open call, so its
speed is unmeasured until someone runs it on an elevated Windows prompt.

## What `denied` means

`denied` counts folders or files that Arkos could not read, usually for lack of permission. They
are counted and skipped, never guessed, and the report warns when the number is above zero. Run
elevated, or accept the gap. `skipped` counts entries left out on purpose: your `--exclude`
patterns, Arkos's own data folder when it sits inside the scan, and names that are not valid
Unicode (these are listed in the log).

## Renames that differ only in case

`diff` compares paths byte for byte. Renaming `Foo` to `foo` therefore shows as one removed entry
and one added entry, for folders and files alike.

## How sizes are counted

Sizes are logical and counted per name. A hardlinked file can be counted more than once by the
walk, and allocated size on disk can be smaller for compressed, sparse or cloud placeholder files.
Every report says so. The ledger layout for other tools is in `schema-contract.md`.
