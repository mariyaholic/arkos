# WORKSTACK

## Threads
- **Unix volume lock** (PR mariyaholic/arkos#2, draft)
  - From→To: claude.impl → claude.review
  - Done: non-Windows lock uses `File::try_lock` (flock); `rust-version = "1.89"`
  - Next: review and merge; run `cargo test` once on Windows
  - Files: `src/lock.rs`, `Cargo.toml`
  - Verify: `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test` (7/7 on Linux)
  - Blockers: none

## Verification
- (runbook pending)
