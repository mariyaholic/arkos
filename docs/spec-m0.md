# Arkos — M0 build contract (snapshot scanner and diff)

Author: Buffy (freebuff.impl). Owner: Maria. Date: 2026-11-12. Status: implementation specification.
Registry status for this app is `proposed`; every number below is an acceptance target with a
measuring method, not a measurement.

Authority: [atlas card `mh-shimatsu-001`, suite `shimatsu`](../app-atlas/shimatsu.md) ·
[suite architecture §2 Kern, §3 classes, §4 ladder, §7 build order](../app-suite-architecture.md) ·
[Clipbase M0](clipbase-m0.md) (shared-store discipline) · [Moji hardening](moji-hardening-0.3.md)
(probe/exit convention).

Registry claim to beat, verbatim: **"1M files < 10 s on NTFS via MFT · <= 60 MB peak by streaming
into SQLite · fallback walk 200k files/s"**.

## 0. Scope, and the registry kill read strictly

M0 is a **scanner**: enumerate a volume (or a subtree) once, stream the result into SQLite, roll the
directories up, and answer four questions from that snapshot — largest files, oldest files, biggest
directories, and what changed between two snapshots. It is read-only. It never deletes, never
quarantines, never touches file contents, and has no network code at all.

Out of M0: DupliCut's hashing, Haki's cleaning, Tachiage's startup inventory, the treemap *window*
(the M0 artefact is a static HTML file), per-directory growth alerts (H2), scheduled snapshots (H3).

### The kill criterion is already partly true, and the spec says so

The card's kill: *"If MFT reading needs a driver or admin on current Windows; then the fallback walk
is the product and the 10 s claim is withdrawn."* Opening `\\.\C:` for `FSCTL_ENUM_USN_DATA` needs a
volume handle that a standard user cannot obtain without elevation — no driver, but elevation all
the same. Pretending otherwise would make the headline number a lie on the default install, so this
contract takes the card at its word:

- **The product is the fallback walk.** A plain `arkos scan --volume C:` runs unelevated, at the walk
  budget. That is the number a reviewer should hold us to.
- **MFT is an accelerator behind one explicit flag** (`--mode mft`, refused with exit 4 when not
  elevated), reported separately in the probe and in every snapshot row (`mode`). The 10 s claim is
  scoped to *that* path: elevated, warm NTFS, NVMe.
- The scan never requests elevation itself, never relaunches itself elevated, and never writes a
  UAC prompt into an installed scheduled task. `--mode auto` (the default) picks `mft` **only** when
  the process is already elevated and the volume is NTFS.

This is the honest reading of the kill, and it is the reason the two numbers in §6 are separate
gates rather than one.

## 1. Crate layout against the Kern spine

```text
arkos/
  Cargo.toml
  src/main.rs                 # cli::run -> exit::Exit::code
  src/exit.rs                 # NEW-style shared taxonomy (§3), one table, uniqueness test
  src/probe.rs                # Kern probe document (§2)
  src/cli.rs
  src/config.rs               # kern-core restricted TOML (retention, excludes, defaults)
  src/paths.rs                # kern-core paths; nothing written outside the data dir
  src/logging.rs              # kern-core bounded rolling log
  src/volumes.rs              # volume enumeration, filesystem type, free space, elevation check
  src/lock.rs                 # per-volume advisory lock (§4.3), kern-core lockfile
  src/scan/mod.rs             # Scanner trait, batching writer, progress counters, abort handling
  src/scan/walk.rs            # portable fallback; bounded worker threads, no async
  src/scan/mft.rs             # cfg(windows, feature="mft"): FSCTL_ENUM_USN_DATA, two-phase
  src/scan/paths.rs           # \\?\ long paths, junction/reparse policy, permission-denied tally
  src/db/mod.rs               # open policy (kern-db), migrate lock, user_version guard
  src/db/0001.sql             # Arkos owns 0001 (§4.2)
  src/db/retention.rs         # prune + abandoned-scan sweeper + incremental_vacuum (§4.4)
  src/rollup.rs               # bottom-up directory totals, one descending-depth pass
  src/diff.rs
  src/top.rs
  src/tree.rs                 # squarified layout -> static HTML/SVG, no JS framework
  src/panel.rs                # cfg(feature="panel") egui panel, M1; never in the M0 gate
  tests/
  examples/gen_tree.rs        # fixture generator: --files N --dirs M --depth D
  scripts/measure.ps1
```

```toml
[features]
default = ["mft"]
mft   = ["dep:windows"]      # elevated acceleration only; the walk must never depend on it
panel = ["dep:eframe"]       # M1 window; off in M0 so the peak budget measures the scanner
ipc   = []                   # M2 panel <-> scan progress over kern-ipc

[dependencies]
kern-core  = { path = "../kern/core" }
kern-db    = { path = "../kern/db" }
kern-probe = { path = "../kern/probe" }
kern-ipc   = { path = "../kern/ipc" }          # only with feature "ipc"
rusqlite = { version = "0.37", features = ["bundled"] }
clap = { version = "4", features = ["derive"] }
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.60", features = ["Win32_Foundation", "Win32_Storage_FileSystem",
                                          "Win32_System_IO", "Win32_System_Threading"] }
```

Rules that are structural, not stylistic:

- **`--no-default-features` must still scan.** The walk path may not be compiled conditionally; only
  `mft.rs` is gated. A build without `windows` reports `features: ["walk"]` in the probe and exits 5
  on `--mode mft` — it does not silently fall back.
- No async runtime, no ORM, no `walkdir` (the walk is the product and its error policy is the point),
  no `rayon` (three `std::thread` workers are enough and stay within the memory gate), no HTTP crate
  anywhere.
- `eframe` is behind `panel` and off by default: a 60 MB gate must measure the scanner, not a GPU
  context. Moji's measured 14.8 MB egui idle is the warning.

## 2. Probe document

`arkos probe`. The measured half is written by `scripts/measure.ps1` after a real scan of the
1M-file fixture; it is never estimated.

```json
{"v":1,"app":"arkos","version":"0.1.0","class":"on-demand",
 "features":["walk"],
 "compiled":{"mft":false,"panel":false,"ipc":false},
 "budgets":{"mft_1m_ms":10000,"walk_files_per_s":200000,"peak_bytes":60000000,
            "db_bytes_per_1m_snapshot":75000000,"diff_1m_ms":2000,"binary_bytes":8000000},
 "measured":{"mft_1m_ms":null,"walk_files_per_s":null,"peak_bytes":null,
             "db_bytes_per_1m_snapshot":null,"diff_1m_ms":null,"binary_bytes":null,
             "fixture":"gen_tree --files 1000000 --dirs 120000 --depth 12",
             "at":null,"method":"scripts/measure.ps1"}}
```

Every snapshot row also carries its own `mode` and `scan_ms`, so a user can see which path produced
a number without trusting the probe.

## 3. Verb surface and the shared 0–6 taxonomy

| Invocation | Argument shape | Notes |
|---|---|---|
| `arkos scan --volume C:` | volume letter or `\\?\Volume{guid}`; optional `--path <subdir>`, `--label TEXT`, `--mode auto\|mft\|walk`, `--exclude GLOB` (repeatable) | read-only; a scan is one snapshot |
| `arkos list [--volume C:] [--limit N]` | N default 20 | newest first, includes running/abandoned |
| `arkos diff <a> <b> [--min-size B] [--top N] [--out PATH]` | `a`,`b` are snapshot ids; `--out` must not exist | grows/shrinks/new/deleted, per directory and per file |
| `arkos top --snapshot N --by size\|mtime\|count [--limit N]` | — | files or dirs (`--dirs`) |
| `arkos tree --snapshot N --out PATH [--depth D]` | static HTML + SVG; `--out` must not exist | no server, no JS framework |
| `arkos prune --keep N [--volume C:] [--yes]` | N 1–12 | deletes snapshots, never files on disk |
| `arkos db info\|migrate\|vacuum` | — | as Omoi's ledger verbs |
| `arkos paths`, `arkos probe` | — | probe always exits 0 |

| Code | Name | Means here |
|---|---|---|
| 0 | `Ok` | scan/diff/report succeeded; empty result is still 0 |
| 1 | `Io` | DB, filesystem, or report write failure; the snapshot is marked `failed` |
| 2 | `Cli` | bad arguments, unknown volume, `--keep` out of range, missing `--yes` on prune |
| 3 | `Input` | the requested snapshot/id/tree is gone or unusable (pruned, `failed`, different volume) |
| 4 | `Refused` | a decision is required: another scan already running on this volume, `--mode mft` without elevation, target path exists |
| 5 | `Unsupported` | not Windows, non-NTFS volume asked for `mft`, `mft` feature not compiled |
| 6 | `Busy` | the volume lock is held by DupliCut's hash pass or Haki's clean run past `busy_timeout` |

A scan that is interrupted (Ctrl-C, logoff) leaves `state='running'`; the next open marks it
`abandoned` and it is never diffed against. Exit 1 is reserved for failures with no usable snapshot;
a partial directory (permission denied) is *counted*, not fatal — see §5.3.

## 4. Data model

### 4.1 One ledger, and the filename

`shimatsu.sqlite` in the Kern data dir, exactly as `atlas/suites.json` declares. Every member's
tables share the file; ownership is by migration number and table prefix, never by convention.

### 4.2 First writer and migration discipline

**Arkos is the first writer of `shimatsu.sqlite` and owns migration 0001.** The card's
`shimatsu.sqlite · arkos_snapshots, arkos_dirs, arkos_files` is the registry, and it is right: a
suite-wide `shimatsu_*` prefix written by five members would break the ladder's per-prefix writer
rule. The suite chapter's older `shimatsu_snapshots`/`shimatsu_receipts` sketch is superseded and is
corrected with this commit.

| Migration | Owner | Tables | Others may |
|---|---|---|---|
| `0001` | **Arkos** | `arkos_snapshots`, `arkos_dirs`, `arkos_files` | DupliCut/Haki/Mamoru/Kensa: **read only** |
| `0002` | **DupliCut** | `duplicut_groups`, `duplicut_members`, `duplicut_quarantine` | read; never write |
| `0003` | **Haki** | `haki_rules`, `haki_runs` | read; never write |
| `0004` | **Mamoru** | `mamoru_jobs`, `mamoru_receipts`, `mamoru_drills` | Kensa reads receipts |
| `0005`+ | Kensa, Tachiage, Awase, Suzumi, Michi | their own prefixes | read |

Binding rules, the same pattern as `sugu.sqlite` and `kiroku.sqlite`:

1. An app creates only tables it owns, only in the next migration it may author.
2. `PRAGMA user_version` is never lowered; a file with a higher version than the app knows is
   `Refused` (4, `ledger.newer`) and the app prints both versions.
3. Migrations run in `BEGIN IMMEDIATE` under the migrate lock; a concurrent starter exits 6.
4. `0001` is frozen once any other member has shipped against it; a new index on `arkos_files` is a
   later migration by whoever needs it, never an edit to 0001.
5. No member deletes another's rows. `arkos prune` only touches `arkos_*`.
6. **DupliCut, Haki and Mamoru are read-only consumers of the snapshot tables and must not re-walk a
   volume that has a usable snapshot.** They join `arkos_files`/`arkos_dirs` by `(snapshot_id, path)`;
   if they need a hash of a file, they hash that file, not the tree. This is the answer to the
   chapter's open question 3 ("should DupliCut read Arkos's snapshots") — yes, read-only, and the
   chapter's open question is closed by this contract rather than left dangling.

The `suites.json` writer prose used to read *"Scanners write snapshots; the janitor and backup agent
are the only mutating writers, one at a time"*, which undercounts DupliCut — it quarantines files. The
string now names DupliCut, Haki and Mamoru (corrected with this commit), so the registry no longer
contradicts three cards and this spec does not have to explain the contradiction away.

### 4.3 Volume lock

`kern-db`'s write lock is per database; a scan needs a **per volume** mutex so three scanners do not
thrash one spindle. The mutex is a **file**, not a table:

```text
<kern data dir>/locks/volume-<sha256(volume)[..12]>.lock     # e.g. volume-9f2c1a8b3d4e.lock
```

Acquired with the OS advisory lock (`LockFileEx` on Windows, `flock` on unix) and then written with a
diagnostic line — `holder`, `pid`, `started_at`, `argv[0..2]` — for messages only; the line is never
consulted for the decision to acquire. Held for the duration of a scan, hash pass or clean run, and
released on completion, on panic, or by the kernel when the process dies.

Why a file and not a row (this replaces the `arkos_volume_locks` row this spec previously proposed):

1. **Ownership.** DupliCut and Haki must take this lock and must never write `arkos_*` (DupliCut's contract
   says so explicitly). A lock row in a `arkos_`-prefixed table forces every mutating member to write
   another app's tables — a real violation of the ladder's per-prefix writer rule.
2. **Crash safety is the OS's job.** A row needs a heartbeat and a 120 s staleness window, so a
   crashed scan blocks the whole suite for two minutes and a wedged one needs manual override. An
   advisory lock is released by the kernel at process death, so staleness does not exist as a state.
3. **No cross-database write.** The lock is taken before the ledger is opened, so a lock wait cannot
   sit inside a transaction that also holds the migration lock.

The cost is that advisory locks need a filesystem that honours them; on a local volume that is
unconditional, and on a network share it is undefined — so a path on a share is refused (exit 5,
`lock.unsupported_fs`) instead of being silently unserialised. Three scanners on one spindle is the
failure this prevents, and a spinner on a share is exactly where you would notice.

### 4.4 Schema 0001 (concrete)

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE arkos_snapshots(
  id           INTEGER PRIMARY KEY,
  volume       TEXT    NOT NULL,                 -- 'C:'
  root         TEXT    NOT NULL,                 -- 'C:\' or a scanned subtree
  label        TEXT,
  mode         TEXT    NOT NULL CHECK(mode IN ('mft','walk')),
  elevated     INTEGER NOT NULL CHECK(elevated IN (0,1)),
  state        TEXT    NOT NULL CHECK(state IN ('running','complete','failed','abandoned')),
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  files        INTEGER NOT NULL DEFAULT 0,
  dirs         INTEGER NOT NULL DEFAULT 0,
  bytes        INTEGER NOT NULL DEFAULT 0,
  denied       INTEGER NOT NULL DEFAULT 0,       -- directories/files we could not read
  skipped      INTEGER NOT NULL DEFAULT 0,       -- excluded by config
  scan_ms      INTEGER
);
CREATE INDEX arkos_snapshots_volume_at ON arkos_snapshots(volume, started_at DESC);
CREATE UNIQUE INDEX arkos_snapshots_one_running ON arkos_snapshots(volume) WHERE state = 'running';

CREATE TABLE arkos_dirs(
  id          INTEGER PRIMARY KEY,
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  parent_id   INTEGER REFERENCES arkos_dirs(id),
  path        TEXT    NOT NULL,
  name        TEXT    NOT NULL,
  depth       INTEGER NOT NULL,
  mtime       INTEGER,
  bytes       INTEGER NOT NULL DEFAULT 0,        -- rolled up in §4.5
  files       INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX arkos_dirs_path ON arkos_dirs(snapshot_id, path);
CREATE INDEX arkos_dirs_depth ON arkos_dirs(snapshot_id, depth DESC);
CREATE INDEX arkos_dirs_bytes ON arkos_dirs(snapshot_id, bytes DESC);

CREATE TABLE arkos_files(
  snapshot_id INTEGER NOT NULL REFERENCES arkos_snapshots(id) ON DELETE CASCADE,
  dir_id      INTEGER NOT NULL REFERENCES arkos_dirs(id) ON DELETE CASCADE,
  name        TEXT    NOT NULL,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,
  ext         TEXT,                               -- lowercased, no dot; NULL when none
  PRIMARY KEY(snapshot_id, dir_id, name)
) WITHOUT ROWID;
CREATE INDEX arkos_files_size  ON arkos_files(snapshot_id, size DESC);
CREATE INDEX arkos_files_mtime ON arkos_files(snapshot_id, mtime);
CREATE INDEX arkos_files_ext   ON arkos_files(snapshot_id, ext, size DESC);
```

Why files store `(dir_id, name)` and not a full path: 1M rows with a 90-character path costs ~130 MB
per snapshot on its own. Interning the directory once and storing the leaf name brings a row to
~45 bytes, which is what makes four retained snapshots fit in the §6 database gate. Paths are
reconstructed by joining `arkos_dirs`; `arkos_dirs.path` is stored in full because the treemap and the
diff need it.

`ext` is a generated-at-insert column, not a `GENERATED ALWAYS` one: SQLite's expression would need
`instr`/`rtrim` gymnastics per row, and the scanner already has the name in hand.

### 4.5 Rollup

After the stream ends, one pass in Rust:

```sql
-- read once, deepest first; the scanner adds each row into its parent in memory-free fashion
SELECT id, parent_id, bytes, files FROM arkos_dirs
 WHERE snapshot_id = ?1 ORDER BY depth DESC, id DESC;
```

Each child's totals are added into its parent and written back in batches of 5,000
(`UPDATE arkos_dirs SET bytes = bytes + ?2, files = files + ?3 WHERE id = ?1`). No recursive CTE: at
1M files the depth-ordered scan is a straight index walk, and the recursive form re-visits rows.

### 4.6 Diff

Two snapshots of the same volume, matched by path:

```sql
CREATE TEMP TABLE dir_pairs AS
SELECT a.id AS a_id, b.id AS b_id, a.path
  FROM arkos_dirs a JOIN arkos_dirs b ON b.path = a.path AND b.snapshot_id = ?2
 WHERE a.snapshot_id = ?1;

-- grown / shrunk / new directories, by rolled-up bytes
SELECT coalesce(p.path, b.path) AS path,
       coalesce(b.bytes, 0) - coalesce(a.bytes, 0) AS delta
  FROM dir_pairs p
  LEFT JOIN arkos_dirs a ON a.id = p.a_id
  LEFT JOIN arkos_dirs b ON b.id = p.b_id
 ORDER BY abs(delta) DESC LIMIT ?3;
```

Files are diffed the same way through `dir_pairs` plus the `(dir_id, name)` key, which is why the
pair table is built once. A snapshot in `failed` or `abandoned` state is refused (exit 3) — a diff
against a half-written tree is a fake answer.

### 4.7 Retention and GC

| Data | Policy | Statement |
|---|---|---|
| snapshots | last **4 complete per volume** (config `snapshots_keep`, 1–12) | `DELETE FROM arkos_snapshots WHERE id IN (SELECT id FROM arkos_snapshots WHERE volume=?1 AND state='complete' ORDER BY started_at DESC LIMIT -1 OFFSET ?2);` |
| dependent rows | cascade | `PRAGMA foreign_keys=ON` + `ON DELETE CASCADE` (verified by test, not assumed) |
| stale runs | 24 h sweeper on every open | `UPDATE arkos_snapshots SET state='abandoned', finished_at=?1 WHERE state='running' AND started_at < ?2;` |
| logs | 3 × 1 MiB rolling, never in the DB | `kern-core` |
| freelist | after a prune that frees > 32 MiB, one `PRAGMA incremental_vacuum(2048)` | `db/retention.rs` |
| full VACUUM | only `arkos db vacuum`, prints bytes reclaimed | never scheduled |
| file contents | never stored, never copied | by design |

Retention is by **completed snapshot count**, not by age: a monthly user should keep four months of
comparisons, and an hourly user should not keep four thousand. The cap is what makes the database
bounded, and it is checked in the retention test by building five snapshots and asserting the oldest
three of five are gone after `--keep 2`.

## 5. Scan pipeline

### 5.1 Walk (the product)

`std::fs::read_dir` with three worker threads fed by a bounded channel; each worker owns its own
`rusqlite` connection? No — one writer, because SQLite is single-writer and contention would show up
as `Busy`. Instead: workers enumerate and send `(parent_id, entry)` batches of 1,000 over a bounded
`std::sync::mpsc`; the single writer thread inserts inside one transaction per batch. Enumeration
parallelism is where the syscall cost is; insertion is where the lock is.

Batch size 1,000 keeps the peak bounded (1,000 entries × ~200 B ≈ 200 KB) and amortises the
transaction. `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL` via `kern-db`; the scan is
reproducible from the volume, so `NORMAL` is the right durability.

### 5.2 MFT (elevated accelerator)

Two phases, because a single pass cannot resolve names to paths without holding every entry:

1. **Directory pass** — `FSCTL_ENUM_USN_DATA` over the volume, keeping only records whose attributes
   contain `FILE_ATTRIBUTE_DIRECTORY`: `FRN → (parent FRN, name)`. This map is the only structure
   held in RAM; 120k directories at ~64 B ≈ 8 MB.
2. **File pass** — the same enumeration again, resolving each file's parent chain through the map
   and writing `(dir_id, name, size, mtime, ext)` in batches. A record whose parent FRN is missing
   (a directory created between the passes) is counted `denied` and skipped, never guessed.

Two enumerations is cheaper than one enumeration plus a per-file SQLite lookup: the USN read is
sequential and cached, while 1M point queries are not. An in-memory directory map larger than
1,000,000 entries (a pathological tree) drops to phase-1-into-SQLite mode, which is slower and is
reported as `mode='mft-slow'` in the log — the memory gate wins over the speed claim.

Sizes in the MFT pass are the USN record's allocation size; the scanner calls
`GetFileInformationByHandleEx(FileStandardInfo)` only for files above `--detail-over` (default 1 MiB)
where logical vs allocated actually differ. That is an explicit accuracy-for-speed trade and it is
recorded in the snapshot's `mode` note.

### 5.3 Error policy (this is the difference between a tool and a toy)

- `ERROR_ACCESS_DENIED` on a directory: increment `denied`, do not descend, continue. The snapshot is
  `complete` with `denied > 0`, and every report prints `N directories were not readable`.
- A directory that disappears mid-walk: same as denied, counted separately in the log.
- Reparse points (`IO_REPARSE_TAG_MOUNT_POINT`, symlinks): **recorded, never followed**. A junction
  to `D:\` inside `C:\` must not double-count the disk, and a junction loop must not hang the scan.
  They are stored as files of size 0 with `ext = NULL` and a `reparse` flag in the log line.
- Hardlinks: counted per directory entry (each name is a file the user sees). The report says
  "sizes are per name, hardlinks may be counted more than once" — no silent correction.
- Paths ≥ 260 characters: `\\?\` prefix applied on Windows at the API boundary; a path that still
  fails is `denied`.
- Compressed/sparse files: logical size is reported; the snapshot notes that allocated size may be
  smaller. No guessing.

## 6. Budgets, measurement, failing numbers

Class **on-demand**; the architecture's ceiling for the class is 40 MB, and this card is named at
60 MB (the same "named in the card" exception Shirabe/Shiko already use). The gate below is the
card's 60 MB; 40 MB is reported as the suite's stretch target and is **not** a pass condition.

| Metric | Gate | Fails when | Method |
|---|---|---|---|
| MFT scan, 1M files, elevated, warm NVMe | < 10,000 ms | ≥ 12,000 ms | `QueryPerformanceCounter` around the two phases; `GetProcessMemoryInfo` for peak |
| Walk, aggregate | ≥ 200,000 files/s | < 150,000 files/s | 1M-file fixture, median of 3, `files / wall` |
| Peak working set, scan | ≤ 60,000,000 B | ≥ 66,000,000 B | `PeakWorkingSetSize` on the child process handle |
| Peak, 1M-directory pathological tree | ≤ 120,000,000 B | ≥ 150,000,000 B | same; exceeding it switches to `mft-slow` and is reported |
| DB bytes per 1M-file snapshot | ≤ 75,000,000 B | ≥ 90,000,000 B | `page_count × page_size` after `wal_checkpoint(TRUNCATE)` and close |
| Diff of two 1M-file snapshots | < 2,000 ms | ≥ 3,000 ms | wall time of `arkos diff --top 50` |
| Directory rollup, 1M files | < 3,000 ms | ≥ 4,500 ms | wall time inside the same run, logged as `rollup_ms` |
| Cold start to first output (`arkos list`) | < 200 ms | ≥ 400 ms | 10 cold launches, median |
| Release binary | ≤ 8,000,000 B | ≥ 9,000,000 B | artefact size, `strip`, `opt-level="s"`, LTO |
| `arkos tree` for 100k dirs | < 4,000 ms | ≥ 6,000 ms | wall + output size < 40 MB |
| Network | 0 bytes | any | firewall-blocked run must pass unchanged |

Fixture: `examples/gen_tree.rs --files 1000000 --dirs 120000 --depth 12` on a dedicated VHDX
(NTFS, fixed size, defragmented once), plus a 100k-file CI fixture. The generator writes the tree
once and records its manifest hash so the diff numbers refer to a known tree. A 1M-file fixture is
not created in CI on every run; it is a documented local/VHDX setup, which is why the probe's
`measured` block names the fixture.

Two numbers will decide the app: **walk throughput** (the default path on a normal install) and
**database bytes per snapshot** (four retained snapshots must not become a 500 MB ledger).

## 7. Merge points, L-levels, bundles

| Point | Level | Requirement | Status |
|---|---|---|---|
| Owns the suite's snapshot tables | L2 | migration 0001, `arkos_*` prefix, read-only for everyone else | M0 |
| DupliCut reads snapshots | L2 | hash pass takes candidate files from `arkos_files` instead of re-walking | DupliCut H1 |
| Haki reads snapshots | L2 | "space reclaimed" is computed from a before/after snapshot pair | Haki H3 |
| Volume lock | L2 | the lock file in §4.3 is the contract every mutating member honours | M0 |
| Panel | stays L2, own binary | the suite chapter already rules out an L3 window for shimatsu ("a permanent window over a disk treemap is a battery drain"); `panel` is a feature of this app, never merged into a shared surface | M1 |
| Bundle **archivist** | — | Arkos + DupliCut + Mamoru is the "own your disk" half of the archivists' bundle | already a member |
| Bundles **player/operator** | — | deliberately **not** members: a disk treemap in a gaming or server bundle is a tool you run, not a thing installed by default | by design |

## 8. Horizons and kill criteria

Dates assume P0 Kern landed and follow §7's P5 window (8–14 months).

**H1 — 2027-06-30 (M0).** Walk + elevation-gated MFT, schema 0001, rollup, largest/oldest/biggest
tables, static HTML treemap, snapshot diff, prune, probe, volume lock.
*Done when:* the 1M fixture produces a complete snapshot on both paths, the diff of two snapshots
names the same five directories a human finds by hand, `--mode mft` unelevated exits 4 with a
readable reason, and a scan interrupted at 50% is marked `abandoned` on next open.
*Kill check (card):* if elevation cannot be obtained on a standard Windows 11 install even for
`--mode mft`, the MFT path is removed entirely — the walk stays the product and the 10 s claim is
withdrawn, not softened.

**H2 — 2027-12-31 (M1–M2).** egui panel over the same queries (feature `panel`), age/type heat,
growth alerts per directory, CSV/JSON export, scheduled quiet snapshots behind an explicit install
verb, DupliCut/Haki consuming snapshots.
*Kill:* if the panel cannot open a 1M-file snapshot in < 500 ms without loading rows it does not
show, the panel stays a static HTML report and the window is dropped. A treemap that needs a 300 MB
in-memory tree has failed the class.

**H3 — 2028-09-30 (M3).** "What grew since March" as a chart, multi-volume history, per-directory
budgets, and the machine report that joins Arkos with Kensa and Mamoru.
*Kill:* if the scheduled snapshots cannot be made quiet (no scan during a game, no scan on battery),
they stay manual. A background scanner that touches the disk during a raid boss is worse than no
history.

## 9. Test plan, including the failure paths

| Area | Test | Catches |
|---|---|---|
| schema | `migration_0001_idempotent`, `foreign_keys_cascade_on_prune` | orphan rows after retention |
| schema | `newer_user_version_is_4_and_no_write` | a downgrade wiping DupliCut's tables |
| schema | `duplicut_0002_does_not_touch_arkos_tables` | a consumer writing a producer's prefix |
| lock | `second_scan_same_volume_is_4` | three scanners thrashing one disk |
| lock | `lock_released_when_holder_is_killed` | a crashed scan blocking the suite forever |
| lock | `lock_wait_times_out_as_6` | a silent hang instead of a message |
| lock | `different_volumes_scan_concurrently` | over-serialising the whole app |
| scan | `denied_directory_counts_and_completes` | a scan that "succeeds" with a missing subtree |
| scan | `junction_is_recorded_not_followed` | double-counting a whole second volume |
| scan | `long_path_over_260_chars` | `\\?\` handling on deep node_modules trees |
| scan | `hardlink_counted_per_name_and_reported` | silent size under-reporting |
| scan | `interrupted_scan_is_abandoned_not_diffable` | diffing a half-written tree |
| scan | `file_created_during_mft_pass_is_denied_not_guessed` | phantom paths |
| walk | `walk_matches_mft_on_the_same_tree` | the two paths disagreeing by more than the known size caveat |
| rollup | `dir_bytes_equal_sum_of_children` | off-by-one in the depth pass |
| rollup | `depth_pass_is_single_scan` (query plan assertion) | a recursive CTE regression |
| diff | `diff_detects_grow_shrink_new_deleted` | the product's headline report |
| diff | `diff_refuses_failed_snapshot_with_3` | a fake answer |
| retention | `keep_4_prunes_only_completed` | deleting a running scan |
| retention | `prune_does_not_vacuum_automatically` | a stop-the-world VACUUM in a scheduled job |
| report | `tree_html_escapes_file_names` | a file named `<script>` injecting into the report |
| probe | `probe_reports_walk_only_when_built_without_windows` | a build claiming a path it cannot run |
| budget | `walk_throughput_on_100k` (`--ignored`, CI) | the default path rotting |
| budget | `peak_under_60mb_on_1m` (`--ignored`, local VHDX) | the headline memory claim rotting |

```sh
cargo test --all-targets
cargo test --no-default-features --all-targets      # walk-only build must still be whole
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test --release -- --ignored                   # budget gates
node atlas/check-docs.mjs
```

## 10. Risks

- **A scan during a Windows update.** Files vanish and directories rename under the walk. The policy
  is count-and-continue, never retry-forever; the diff then shows deletions that are real enough.
- **Elevation creep.** The card's kill is about admin; the answer is "one flag, one refusal", not a
  manifest that asks for admin at install. If a future feature needs elevation by default, that is a
  new app, not a Arkos release.
- **The DB is the product.** Pruning by count is the only thing standing between a daily snapshot user
  and a 2 GB ledger. The retention test is not optional.
- **`denied` is easy to hide.** Every report must print it; a treemap that silently omits
  `C:\System Volume Information` and looks complete is lying.
- **DupliCut will want to re-walk.** Its hash pass needs paths; the snapshot gives them, but only for
  files that existed at snapshot time. That staleness is the contract: `< 1 h old` is good enough to
  avoid a second full walk, and DupliCut must verify existence before hashing.
- **Two size notions.** USN allocation size vs logical size will differ for compressed and sparse
  files; the snapshot records which it used and never mixes them inside one snapshot.

## 11. Open questions (Arkos only)

1. Should `--path` subtree scans be retained in the same rolling count as full-volume scans, or get
   their own cap? (The spec assumes one cap per volume, which can let small subtree scans push out a
   full snapshot.)
2. Does the treemap HTML belong in the run artefact directory (deletable, like Omoi's export) or in a
   `reports/` folder the user keeps? (Assumed: user-chosen `--out`, never automatic.)
3. Should `ext` be normalised for archive formats (`.tar.gz` → `tar.gz`) in M0 or left as the last
   dot? (Assumed: last dot, with the report grouping by `ext`.)
4. ~~When DupliCut ships, does it take the volume lock for a hash pass on a volume with a fresh
   snapshot, or is reading a snapshot lock-free?~~ **Answered by [DupliCut's contract](duplicut-m0.md)
   §4.4:** reading the snapshot and hashing files is lock-free; only the window in which it *moves*
   files into quarantine takes the lock, and it states that rule from the other side. The lock file
   in §4.3 exists precisely so a read-only consumer can share it without writing `arkos_*`.
