# Elevated runs and the data directory

Status: design, not implemented. Finding 2 of the 2026-11-18 security review.

## Problem

`--mode mft --experimental` needs an elevated token, and a user may run any verb from an
Administrator shell. An elevated Arkos still keeps its files under the user's own data directory,
`%LOCALAPPDATA%\mariyaholic\shimatsu` (or `ARKOS_HOME`), which unelevated code running as the same
user can change. That code can:

1. Replace `logs\`, `locks\` or the home folder itself with a junction to a protected directory.
   The elevated process then creates `arkos.log`, `<hash>.lock`, `<hash>.lock.hb`,
   `shimatsu.sqlite` and its `-wal` and `-shm` there, and renames the log files during rotation.
   The names are fixed and the log content is partly chosen through file names, which is enough to
   plant or overwrite files in places an unelevated user cannot write.
2. Plant a crafted ledger or `-wal` file for the elevated process to open. RUSTSEC-2026-0459 was one
   such bug and is fixed by SQLite 3.54.1 (`26ea4a0`); the next one will look the same.

Microsoft does not treat the boundary between a user's unelevated and elevated tokens as a
security boundary, so this is defence in depth. It matters here because the MFT reader is the one
feature that asks people to elevate.

## Options considered

| Option | Closes 1 | Closes 2 | Cost |
|---|---|---|---|
| A. Check for reparse points before use | Only before the check (race) | No | Small |
| B. Separate admin-only ledger under `%ProgramData%` | Yes | Yes | Splits the ledger; DupliCut and `diff` see two databases |
| C. Elevated helper that only reads the MFT and streams records to an unelevated parent | Yes | Yes | Large: UAC launch, an ACL'd named pipe, a wire format |
| D. Check, then pin the directories open while elevated, plus defensive SQLite | Yes | Reduces | Small to medium |

## Decision proposed: D

D closes the junction attack without moving data, keeps one ledger, and adds no dependency. C is
the long-term answer if the MFT reader becomes a default feature, and fits the `ipc` feature that
spec-m0 reserves for M2.

### D1. Guard runs only when elevated

`volumes::is_elevated()` already exists. When it is false nothing below runs, so ordinary use pays
nothing. The guard runs in `cli.rs` right after `Paths::resolve` and before `ensure_dirs`, and
covers every verb, because any verb can be run from an Administrator shell.

### D2. Pin the directory chain

For each directory from the data root down (`%LOCALAPPDATA%`, `mariyaholic`, `shimatsu`, then
`locks` and `logs`; with `ARKOS_HOME`, from that folder down):

1. Create it if missing (`CreateDirectoryW`; an existing reparse point fails the next step).
2. Open it with `CreateFileW(FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE,
   FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)`, deliberately without
   `FILE_SHARE_DELETE`.
3. Refuse (exit 4, `Refused`) when `GetFileInformationByHandleEx(FileAttributeTagInfo)` shows
   `FILE_ATTRIBUTE_REPARSE_POINT`, or when `GetFinalPathNameByHandleW` differs from the expected
   path (ignoring case and the `\\?\` prefix).
4. Keep every handle open in a `PinnedDirs` value that lives until the process exits.

A directory cannot be renamed, deleted or turned into a junction while another handle on it lacks
`FILE_SHARE_DELETE`, so the chain checked in step 3 is the chain used for the rest of the run. This
removes the race that makes option A unsafe.

The refusal says which folder is a reparse point and that elevated runs refuse to write through
one.

### D3. Check the fixed files

Before the elevated process opens `shimatsu.sqlite`, `-wal`, `-shm`, `arkos.log`, `arkos.log.1`
to `.3`, and the lock and heartbeat files, each one that exists is opened with
`FILE_FLAG_OPEN_REPARSE_POINT` and refused when it is a reparse point or has more than one hard
link (`nNumberOfLinks > 1`). With the directories pinned, a file cannot be swapped for a link after
this check except by deleting it inside a pinned directory and creating a link in its place. Arkos's
own files are opened by name right after the check, which leaves a window of microseconds. A
symbolic link needs `SeCreateSymbolicLinkPrivilege` or developer mode, and Windows refuses to
create a hard link to a file the creator cannot write, so the remaining window does not reach
protected files.

### D4. Defensive SQLite while elevated

After opening the ledger in an elevated run, set `SQLITE_DBCONFIG_DEFENSIVE` and
`PRAGMA trusted_schema = OFF`, and run `PRAGMA quick_check` once before the first write; a failure
refuses with exit 4 and names `arkos db vacuum` from an unelevated shell as the way out. This does
not make a hostile ledger safe, but it shuts off the schema features (views and triggers that run
functions) that a planted database would use.

### D5. Unix

Arkos has no elevated mode on Unix, and running it under `sudo` with `HOME` kept is the same
problem. The guard is Windows-only in this batch; the user guide says not to run Arkos under
`sudo`.

## Tests

- Unit: the final-path comparison (case, `\\?\` prefix, trailing separator).
- Windows, not elevated: the guard is a no-op (every CI run covers this).
- Windows, elevated (`#[ignore]`, beside `mft_windows.rs`): a junction at `logs` makes `scan`
  exit 4; a hard link at `arkos.log` makes it exit 4; a pinned `logs` cannot be renamed by another
  process while the scan runs.
- Under Wine: the pinning and refusal code paths with the elevation check forced on through a
  test-only seam.

## Open question for the owner

Should an elevated run that finds a reparse point refuse (proposed), or move to a fresh data
directory under `%ProgramData%` that it creates with an admin-only ACL (option B for that run only)?
Refusing is simpler and keeps one ledger; the user fixes the folder and runs again.
