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
| 2 | cli | Bad arguments, `--mode mft` without `--experimental`, or `prune` without `--yes` or `--dry-run`. |
| 3 | input | A snapshot does not exist, is not complete, or the two snapshots are on different volumes. |
| 4 | refused | `--mode mft` without an elevated token, an existing output file, or another scan on the volume. The ledger is also refused when a newer Arkos wrote it. |
| 5 | unsupported | `--mode mft` on a build without the feature or on a volume that is not NTFS. |
| 6 | busy | The volume lock or the database stayed busy past the timeout. |

## Why the walk is the default

The walk works on every filesystem and needs no elevation, and `--mode auto` always uses it. The
snapshot records the path that actually ran. A scan of a subfolder always walks, because the MFT
reader covers a whole volume.

## The experimental MFT reader

`--mode mft` reads the NTFS master file table through the USN enumeration call. It has never run
on a real volume, so it is off unless you ask for it: `--mode mft` alone exits 2 and names the
flag, and `--mode mft --experimental` runs it and prints one line on stderr saying it is
experimental and untested. It needs an elevated prompt and a volume root such as `X:`. The
snapshot still records `mode` as `mft`. If the volume cannot be opened or has more than a million
folders, `--mode mft` exits 5 and says why.

Try it on a small USB stick or a mounted VHD first, then compare it with the walk:

```
scripts\compare-mft.ps1 -Root X:\
```

The script scans the root with the walk and with the MFT reader, runs `arkos diff` between the
two snapshots, and prints the file and folder counts, the total bytes, and the largest paths that
appear on one side only. Some gaps are expected:

- Hardlinks: the MFT reader counts a hardlinked file once, and the walk counts every name.
- Alternate data streams: the two readers may size a file with extra streams differently.
- NTFS metafiles such as `$MFT` and `$LogFile`: the MFT reader can see them and the walk cannot.
- Folders the walk was denied: the walk counts them in `denied` and skips them, and the MFT reader
  lists them.
- Files that changed between the two scans.

Any other gap is worth reporting. The scan log (`arkos paths` shows where it is) records the USN
range seen, the record and folder counts of each phase, the parent numbers of orphaned folders,
the time of each phase and any Win32 error code, in an `mft_diag` line.

Experimental builds are published as prereleases: a tag with a hyphen, such as `v0.2.0-exp.1`,
makes a GitHub prerelease that is not marked latest.

## When another scan holds the lock

Only one scan runs on a volume at a time. A running scan writes a small file next to its lock
(`<lock file>.hb`) at most every 5 seconds from its own loop. If you start a second scan and the
first still holds the lock, the error (exit 6) names the first scan's process id. When that scan
has not progressed for over 120 seconds, the error says it looks stalled and suggests ending that
process; Arkos does not end it for you. `arkos status` lists each running scan as `running` or
`stalled (N s)`.

## Cloud placeholders and long paths

Files that a cloud provider keeps online (OneDrive and similar) carry a recall or offline
attribute. The scan counts them as `placeholders` in the text and JSON output and in the log, and
records their logical size like any other file, so a placeholder adds its full size to the totals
although it may use no disk space. Arkos never opens a placeholder for reading, so it does not
download one. On Windows the walk reaches paths longer than 260 characters.

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
