# Schema 0002 plan

Decision 2 in [`h1-1.md`](h1-1.md): plan schema 0002 to bring the ledger under the 75 B per file
gate, ship it with a matching DupliCut change, and merge no 0002 code before DupliCut is ready.
This file is the plan. It holds no migration code and changes nothing in `src/db/0001.sql`, which
stays frozen.

## Summary

| | Today (0001) | Target (0002) |
|---|---|---|
| Bytes per file, measured | 160 | 42 (prototype, see below) |
| Gate | 75 | 75 |
| Reaches the gate | no, 2.1 times over | yes, with margin for names up to about 45 characters |

The 245 B per file in `measurements.md` and `architecture-h1.md` came from a script error:
`scripts/measure.sh` and `scripts/measure.ps1` divided the database size by 2 while the database
held 3 snapshots. The correct figure for 0001 is about 160 B per file. The scripts are fixed in the
same batch. The plan below is sized against the corrected figure.

## 1. Today: where 160 B per file goes

The numbers come from `dbstat` on a fresh ledger with two snapshots of the 100k-file, 12k-directory
fixture (hash `7246fc3c7d94762b`), 200,000 file rows in all. File names in that fixture average 12
characters.

| Object | Bytes per file row | Why |
|---|---|---|
| `arkos_files` (WITHOUT ROWID, key `snapshot_id, dir_id, name`) | 38.4 | The table is its own primary key, so every row carries the name once, plus the sizes. |
| `arkos_files_ext` (`snapshot_id, ext, size DESC`) | 33.2 | An index entry holds its key plus the whole primary key again: `snapshot_id`, `dir_id` and `name`. |
| `arkos_files_mtime` (`snapshot_id, mtime`) | 29.7 | Same repeated primary key. |
| `arkos_files_size` (`snapshot_id, size DESC`) | 27.6 | Same repeated primary key. |
| `arkos_dirs` rows | 14.2 | 0.12 directories per file, 115 B per row, with `path` stored in full (78 B on average). |
| `arkos_dirs_path` (`snapshot_id, path`, unique) | 12.8 | A second full copy of every path. |
| `arkos_dirs_bytes` and `arkos_dirs_depth` | 3.8 | |
| Total | 160 | 31.95 MB for 200,000 rows |

Two causes account for most of it. In a WITHOUT ROWID table each secondary index entry repeats the
whole primary key, which includes the name, so three indexes store the name three more times. And
every directory path is stored twice, once in the row and once in the unique index, although a path
is already the parent's path plus a name.

The ext and mtime indexes are not read by any Arkos query (`top`, `diff`, `tree`, `rollup` and
`prune` do not use them). The size index serves `top` and the file ranking in `tree`.

## 2. Target layout

```sql
-- sketch only; the migration text is written when the DupliCut change is ready
CREATE TABLE arkos_exts(
  id  INTEGER PRIMARY KEY,
  ext TEXT NOT NULL UNIQUE
);

CREATE TABLE arkos_dirs(
  id          INTEGER PRIMARY KEY,
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  parent_id   INTEGER REFERENCES arkos_dirs(id),
  name        TEXT    NOT NULL,          -- the root row holds the last component of the root
  depth       INTEGER NOT NULL,
  mtime       INTEGER,
  bytes       INTEGER NOT NULL DEFAULT 0,
  files       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX arkos_dirs_depth ON arkos_dirs(snapshot_id, depth DESC);
CREATE INDEX arkos_dirs_bytes ON arkos_dirs(snapshot_id, bytes DESC);

CREATE TABLE arkos_files(
  id     INTEGER PRIMARY KEY,            -- the rowid
  dir_id INTEGER NOT NULL,               -- no REFERENCES clause, see "Deleting a snapshot"
  name   TEXT    NOT NULL,
  size   INTEGER NOT NULL,
  mtime  INTEGER NOT NULL,
  ext_id INTEGER NOT NULL DEFAULT 0      -- 0 means no extension; otherwise arkos_exts.id
);
CREATE INDEX arkos_files_big ON arkos_files(size DESC) WHERE size >= 1048576;

CREATE TABLE arkos_ext_totals(
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  ext_id      INTEGER NOT NULL,
  files       INTEGER NOT NULL,
  bytes       INTEGER NOT NULL,
  PRIMARY KEY(snapshot_id, ext_id)
) WITHOUT ROWID;
```

What changes, item by item:

- **A rowid on `arkos_files`.** The table stops being its own primary key, so no index repeats the
  name. `snapshot_id` is no longer stored on file rows; it is `arkos_dirs.snapshot_id` reached
  through `dir_id`. The `(snapshot_id, dir_id, name)` uniqueness check goes away. The scanners
  already write each name once per directory.
- **`ext` as an integer.** `arkos_files.ext_id` points into `arkos_exts`, a dictionary of a few
  hundred strings. The value 0 means "no extension" and is stored as a zero-byte integer, so
  extensionless files and reparse points cost nothing. `0001` stored the text on every row and in
  its index.
- **No mtime index.** Nothing reads it. A "changed since" query becomes a scan of the snapshot's
  files, which at 1M rows costs under 0.1 s.
- **A partial size index.** `arkos_files_big` holds only files of at least 1 MiB (0.9% of rows in the
  fixture; expect 1% to 3% on real disks). A query uses it only when its `WHERE` clause repeats
  `size >= 1048576`, so `top --files` and the tree's file list add that predicate. A request for
  more big files than exist, or for small files, falls back to a scan with a top-N heap.
- **Extension totals at rollup.** `arkos_ext_totals` holds files and bytes per extension and
  snapshot, written by `rollup_snapshot` in one `INSERT ... SELECT ... GROUP BY` after the
  directory rollup. It replaces the `(snapshot_id, ext, size DESC)` index for the question "how much
  is in each extension". The question "largest files of one extension" scans the snapshot's files
  filtered by `ext_id`.
- **`arkos_dirs.path` rebuilt from `parent_id`.** The column and `arkos_dirs_path` are gone. A
  directory's id is always larger than its parent's id in both scanners (the walk inserts a parent
  before its children, and the MFT reader inserts in depth order), so one pass over a snapshot's
  directories in id order builds every path: the root's path is `arkos_snapshots.root`, and a child
  is its parent's path, a separator, and its name. The separator is `\` when the root contains a
  backslash and `/` otherwise, and no separator is added after a root that already ends in one
  (`C:\`).

### Byte estimate per file

Measured by loading the same two-snapshot fixture into the target layout with SQLite 3.53 and
reading `dbstat` (a prototype in a scratch database, not code in the repository):

| Object | Bytes per file row |
|---|---|
| `arkos_files` rows | 34.2 |
| `arkos_dirs` rows and their two indexes | 7.3 |
| `arkos_files_big` | 0.2 |
| `arkos_exts`, `arkos_ext_totals` | 0 |
| Total | 41.7 |

A ledger of 1M files at this shape is 42 MB per snapshot, against 160 MB now. The row cost is
about `22 + n` bytes for a name of `n` characters (cell header, rowid, record header, directory id,
size, mtime, extension id), so the estimate moves one byte per character of average name length.
The gate is reached with 0.12 directories per file for names averaging up to about 45 characters:
`34.2 + 7.3 + 0.2 + (n - 12)` stays at or under 75 up to `n = 45`. Real trees average around 20 to
25 characters, which gives 50 to 55 B.

Two shapes are worse. The 100k-directory fixture (one directory per file) costs about 93 B per file
in the target, because each directory row and its two indexes cost 59 B; it is a synthetic extreme
and was about 435 B per file in 0001. A tree with many short names and few directories is cheaper than the
fixture.

## 3. Deleting a snapshot

`arkos_files.dir_id` carries no `REFERENCES` clause. With one, SQLite would scan `arkos_files` once
for every directory row deleted, because `dir_id` has no index, and a prune would take quadratic
time. Instead `prune` and `fail_snapshot` delete in an explicit order inside one transaction:

1. `DELETE FROM arkos_files WHERE dir_id IN (SELECT id FROM arkos_dirs WHERE snapshot_id = ?1)`.
   The plan is one scan of `arkos_files` with the directory ids held in a temporary set. Measured at
   200k rows (100k deleted): 0.08 s.
2. Delete the snapshot row; `ON DELETE CASCADE` removes its directories and extension totals.

An index on `dir_id` would keep the foreign key but costs 10.9 B per file, which moves the 42 B
estimate to 53 B and the break-even name length from 45 to about 34 characters. A read of one
snapshot's files is also faster without it: a full scan joined to `arkos_dirs` by primary key took
9 ms per 100k rows, against 31 ms through the index.

## 4. What DupliCut must switch to

DupliCut reads `arkos_files` and `arkos_dirs` (spec section 4.2, `schema-contract.md`). The table
names stay, so an old query fails at prepare time with "no such column" and never reads wrong data.
Three things change for a reader:

| 0001 reads | 0002 replacement |
|---|---|
| `arkos_files.snapshot_id` | `JOIN arkos_dirs d ON d.id = f.dir_id`, then `d.snapshot_id` |
| `arkos_files.ext` (text) | `LEFT JOIN arkos_exts e ON e.id = f.ext_id`, then `e.ext` (NULL for `ext_id = 0`) |
| `arkos_dirs.path` and the `(snapshot_id, path)` join key | rebuild from `parent_id` (below) |
| `arkos_files` primary key `(snapshot_id, dir_id, name)` | `arkos_files.id`, unique across all snapshots |
| `ORDER BY size DESC` on the whole table | add `AND size >= 1048576` to use `arkos_files_big` |
| `ORDER BY ext, size DESC` | `arkos_ext_totals` for sums; a filtered scan for the largest files |

Migration 0002 also creates two compatibility views that restore the 0001 column names, so a
consumer can move by renaming the table in its query and then optimise:

```sql
CREATE VIEW arkos_files_v AS
SELECT d.snapshot_id, f.dir_id, f.name, f.size, f.mtime, e.ext
FROM arkos_files f
JOIN arkos_dirs d ON d.id = f.dir_id
LEFT JOIN arkos_exts e ON e.id = f.ext_id;
```

`arkos_files_v` filters well: `WHERE snapshot_id = ?` becomes a scan of `arkos_files` with a
primary-key probe per row. `arkos_dirs_v` restores `path` with a recursive common table expression
over `arkos_dirs` joined to `arkos_snapshots.root`. SQLite cannot push a snapshot filter into a
recursive expression, so the view computes the paths of every snapshot each time (about 15 ms for
12k directories, proportional to all stored directories). A consumer that reads large ledgers
should instead read `SELECT id, parent_id, name FROM arkos_dirs WHERE snapshot_id = ?1 ORDER BY id`
and build the path map in memory, the way Arkos does. Both forms are written into
`schema-contract.md` when 0002 lands.

Arkos's own queries change too: the writers in `scan/walk.rs` and `scan/usn.rs`, `rollup.rs`,
`top.rs`, `tree.rs`, `diff.rs`, `db/retention.rs`, `db info`, and the golden list in
`tests/schema_contract.rs`. `diff` joins directories across snapshots by path, so it builds a
temporary table of `(path, dir_id)` for each side and joins those; the diff gate (under 2 s at 1M
files, 1.9 s on the 100k-directory fixture today) needs a measurement before 0002 is accepted.

## 5. The version gate

Arkos refuses a ledger whose `user_version` is above `KNOWN_MAX_VERSION` (exit 4,
`ledger.newer`). The decision sets `user_version` = 2 for the new layout and `KNOWN_MAX_VERSION` to
2, so an Arkos from before 0002 refuses a migrated ledger instead of reading a layout it does not
know. That is the downgrade protection, and it needs no extra code.

**Collision to settle before any code.** Spec section 4.2 assigns migration number 0002 to
DupliCut (`duplicut_groups`, `duplicut_members`, `duplicut_quarantine`), and `user_version` is
one counter for the whole file. A ledger that DupliCut has already migrated has `user_version` = 2
with the 0001 Arkos layout. Arkos would then see version 2 and either refuse it (today) or take it
for the new layout. Owner decision needed, with two sound options:

- Arkos takes the next free number (0003 in the suite table, or whatever DupliCut has shipped)
  and the gate reads `user_version >= N`, where N is that number. Haki's 0003 would move.
- DupliCut renumbers, so Arkos 0002 comes before DupliCut's tables.

Either way the code should also detect the layout directly and not trust the number alone: the
new layout is the one where `arkos_exts` exists (`SELECT 1 FROM sqlite_master WHERE name =
'arkos_exts'`). Arkos opens a ledger when the version is known and the layout marker agrees with the
version, and refuses it with a message naming both when they disagree.

The migration is explicit. `scan`, `prune` and the other verbs do not run it on a ledger that holds
data at the old layout: they exit 4 with "run `arkos db migrate`" because a migration can take
minutes and needs the safety steps below. A new empty ledger is created at the new layout directly.

## 6. Migration safety

The migration runs from `arkos db migrate` only. It follows this order and stops at the first
failure.

1. **No scan running.** Read the `running` snapshots and check each volume lock with
   `VolumeLock::is_free`. If any lock is held, exit 6. The migration then takes every volume lock
   in the ledger (sorted order, so two migrations cannot deadlock) for its whole run, so no scan
   starts halfway.
2. **Free space.** Required free space on the ledger's volume is `2 * (db_bytes + wal_bytes) + 64
   MiB`. The backup needs 1x. The migration needs about 0.5x on top: the new tables are about 0.26x
   the old size, written once to the WAL and once to the file, and the closing `VACUUM` needs a
   temporary copy of the new size. On Windows the check uses `GetDiskFreeSpaceExW`, which the
   existing `windows-sys` dependency already exposes (`Win32_Storage_FileSystem`). The standard
   library has no free-space call and no new dependency is allowed, so on other platforms the
   check is skipped and the backup step failing with "no space" is the guard. If the check fails,
   exit 1 with the numbers and change nothing.
3. **Backup.** Copy the ledger with the `rusqlite` `backup` feature, which `Cargo.toml` already
   enables: `Backup::new(&src, &mut dst)` then `run_to_completion` in steps of 1,000 pages, into
   `shimatsu.sqlite.pre-0002-<unix seconds>.bak` in the ledger's folder. This copies a consistent
   image through SQLite and needs no file-level lock handling for the WAL. Then open the copy
   read-only, run `PRAGMA integrity_check`, and compare the row count of each Arkos table with the
   source. Any difference deletes the copy and stops. The path of a verified backup is printed.
4. **Convert in one transaction.** `BEGIN IMMEDIATE`; create the new tables under temporary names;
   fill `arkos_exts` from `SELECT DISTINCT ext`; copy directories in id order (keeping ids, so
   `parent_id` and `dir_id` stay valid) and files ordered by `(snapshot_id, dir_id, name)` so the
   rowids ascend and the pages pack full; build `arkos_ext_totals` per snapshot; drop the old
   tables and their indexes; rename the new ones; create the partial index and the views; verify
   that per-snapshot file counts and byte sums equal the `arkos_snapshots` and `arkos_dirs`
   totals; set `user_version`; `COMMIT`. Any error, including a verification mismatch, runs
   `ROLLBACK` and leaves the 0001 ledger as it was, with `user_version` unchanged. The transaction
   is atomic, so there is no half-migrated state to repair.
5. **Reclaim space.** After the commit, `PRAGMA wal_checkpoint(TRUNCATE)` and `VACUUM` shrink the
   file to the new size. A failure here leaves a correct, larger ledger and prints a note; running
   `arkos db vacuum` later finishes the job.
6. **Final check.** `PRAGMA integrity_check` and the same count comparison against the backup's
   numbers. On a mismatch, print the restore instructions and exit 1.

**Rollback.** Before the commit, rollback is the transaction's. After the commit, the way back is
the backup: close every connection, delete `shimatsu.sqlite-wal` and `shimatsu.sqlite-shm`, and
copy the `.bak` file over `shimatsu.sqlite`. A binary from before 0002 refuses the migrated
ledger, so a downgrade of the program always goes together with this restore. The backup is never
deleted by Arkos; the user guide tells the user to delete it once DupliCut has run on the new
ledger. `arkos db migrate` prints the backup path on success.

**Tests the migration needs before it merges.** A ledger built by the 0001 code with several
snapshots, a failed snapshot and a reparse point migrates, and every `top`, `diff` and `tree`
result is identical before and after. A forced failure at each numbered step leaves the ledger at
version 1 and readable. A destination with too little space stops at step 2 or 3 with the ledger
untouched. A migrated ledger is refused by `KNOWN_MAX_VERSION = 1`. A held volume lock stops the
migration with exit 6.

## 7. Merge gate

No 0002 code lands until all of these hold:

1. The owner has settled the migration number (section 5).
2. DupliCut has a branch that reads the new layout (section 4) and passes its own tests against a
   ledger migrated by the Arkos migration.
3. The diff gate is measured on the new layout at 100k files with 12k and with 100k directories,
   and holds.
4. `schema-contract.md` and `tests/schema_contract.rs` carry the new column list in the same pair
   of commits as DupliCut's change.
