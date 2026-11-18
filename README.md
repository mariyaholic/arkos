# Arkos

_made by [maria](https://github.com/mariyaholic)_

I made Arkos to scan a Windows volume or folder once, store the result in
SQLite, and answer four questions from that snapshot: the largest files, the
oldest files, the biggest folders, and what changed between two snapshots.
It exports a static HTML treemap that opens offline. It reads only; it never
deletes, moves or opens file contents, and it has no network code. I'm fond
of this one.

Atlas card `mh-shimatsu-001`, suite Shimatsu. DupliCut reads the snapshots
Arkos writes.

## Quickstart

```powershell
cargo build --release
.\target\release\arkos.exe scan --volume C: --path C:\Users\me\Downloads --mode walk
.\target\release\arkos.exe list
.\target\release\arkos.exe top --snapshot 1
.\target\release\arkos.exe tree --snapshot 1 --out report.html
.\target\release\arkos.exe diff --help
```

`--mode mft --experimental` reads the NTFS master file table, needs an elevated prompt and is
untested on real volumes; see the user guide before using it. `--mode auto` always walks.

## Data

Snapshots live in `shimatsu.sqlite` under `%LOCALAPPDATA%`. Set `ARKOS_HOME`
to use another folder. `arkos paths` prints every resolved path.

## Status

M0. The treemap is a static HTML file styled with the estate canon tokens; a
treemap window is not in M0. Every budget in the spec is a target with a
measuring method, not a measurement.

## Build and test

```powershell
cargo fmt --check
cargo clippy --all-targets
cargo test
```

## Spec

[`docs/spec-m0.md`](docs/spec-m0.md) is the M0 build contract. Its links to
the app atlas point into the `mariyaholic` repo, where the atlas lives.

## License

MIT

Thank you for reading. ♡
