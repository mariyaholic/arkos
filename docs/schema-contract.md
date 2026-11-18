# Schema contract for consumers

Arkos owns migration 0001 of `shimatsu.sqlite` (`arkos_snapshots`, `arkos_dirs`, `arkos_files`).
DupliCut and any other reader depend on these tables. `tests/schema_contract.rs` pins the column
list, so a change fails there before it reaches a consumer.

## What a reader checks

1. Read `PRAGMA user_version`. Refuse the ledger when it is below 1, because the Arkos tables do
   not exist yet. A higher value is fine for reading: later migrations add tables and leave these
   alone.
2. Read only snapshots with `state = 'complete'`. `running` rows are in progress, and `failed` and
   `abandoned` rows hold no usable data.
3. Join directories and files by `(snapshot_id, path)` as in spec section 4.2. Do not carry
   `arkos_dirs.id` between snapshots; ids are per row, not per path.
4. Open the ledger read-only (`mode=ro`, `PRAGMA query_only = ON`). Arkos is the only writer of
   these tables.

## Meaning of the columns that are easy to misread

- `arkos_dirs.bytes` and `files` are cumulative: they include everything below the directory.
- `arkos_files.size` is logical size. Allocated size can be smaller for compressed, sparse or
  cloud placeholder files.
- A hardlinked file is counted once per name by the walk. The MFT reader sees one record per file
  and counts it once.
- Reparse points are stored as size-0 files with `ext` NULL and are never followed.
- `arkos_files.ext` is lowercase without a dot. `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`,
  `tar.lz`, `tar.lzma` and `tar.z` are stored whole; every other name keeps the text after its last
  dot.
- `arkos_snapshots.mode` is the path that ran (`mft` or `walk`), not the one requested.
- `denied` counts entries that could not be read, and `skipped` counts entries left out on
  purpose (excludes, Arkos's own data directory, names that are not valid Unicode).
- Paths are stored as the scanner built them: `\` on Windows, `/` elsewhere.

## Changing the schema

A change to these tables needs a new migration number, a matching change in DupliCut, and an update
to the golden list in `tests/schema_contract.rs`, all in the same pair of commits.
