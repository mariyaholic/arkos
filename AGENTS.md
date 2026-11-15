# arkos — agent instructions

## What this repo is

Arkos (atlas `mh-shimatsu-001`) scans a volume or folder into `shimatsu.sqlite`, rolls folder sizes
up, diffs snapshots and exports a static HTML treemap. It is read-only and has no network code.

## Build / test

`cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`. Set `ARKOS_HOME` to a scratch
folder when running the binary by hand.

## Layout

`src/scan/` (MFT and walk), `src/db/` (schema 0001, retention), `src/tree.rs` (HTML report),
`tests/contract.rs` (spec gates), `docs/spec-m0.md` (build contract).

## Conventions

The treemap HTML inlines the canon tokens from `design/uika-canonical.css`; keep it free of network
requests and scripts, and escape every path with `html_escape`. Gold is the one accent; red lining
is for warnings only.

## Gotchas

DupliCut reads Arkos's tables. A schema change here breaks DupliCut unless both move together.

<!-- uika:law:begin -->
## Uika estate law (generated — do not edit inside the markers)

This repo is part of the Uika estate. The estate layer lives in the harness homes; this
block carries only what every session here must obey. Edit the source (Uika `estate.json`),
never this block; `uika docs render` regenerates it.

- **Session start:** freshness scan (mtime-first), then read `WORKSTACK.md`.
- **Handoff:** one 6-line block in the WORKSTACK thread — `From→To / Done / Next / Files / Verify / Blockers` (skills/handoff-protocol).
- **Names:** handle `<harness>.<role>`; harness ∈ {claude, codex, opencode, cline, gemini, commandcode, dsh, freebuff, mimo, hermes}; role ∈ {tower, scout, impl, review, judge}. Model is a field, never a name.
- **Branches:** `<feat|fix|docs|chore>/<kebab-topic>`, ≤ 4 words, no agent names. Never push or rewrite main.
- **Posture:** near-autopilot, secrets egress-locked, self-edit lock full.
- **Find things:** `uika find` / `uika where` before exploring; `uika path` / `uika layout` for where a file goes.
- **Prose:** in chat replies, files, code comments, commits and product text, never write a negation-first reveal, significance designation, abstraction agency, deferred noun, coy or thesis-shaped header, aphoristic closer, straw-man knockdown, fragment cadence, dressed metaphor or label-first framing (skills/plain-prose, ADR-0062).
- **Finish:** run the wrap checklist; update `WORKSTACK.md`.

Full laws: Uika `AGENTS.md` and `register/adr/`. Skills: skills/handoff-protocol, find-fast, estate-sync, plain-prose.
<!-- uika:generated from estate.json; edit the source, not this file -->
<!-- uika:docs v1 · sha256:e2ad5a28c274 -->
<!-- uika:law:end -->
