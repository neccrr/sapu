# Contributing to Sapu

Thanks for helping! Sapu deletes files, so the bar for correctness is high.

## Setup

```sh
git clone https://github.com/neccrr/sapu && cd sapu
cargo run --release              # window app
cargo run --release -p sapu-cli  # terminal explorer
```

Requires Rust (stable, 1.95+). Sapu targets macOS; the core and the terminal tools
also build and test on Linux.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same on macOS.

## Ground rules

- **Never call `std::fs::remove_*`.** Everything that removes files goes through
  `sapu_core::cleaner` → `safety` → `journal` → Trash, so it is checked, logged and undoable.
- **Verdicts must be honest.** `Safe` only when the item is rebuilt or re-downloaded
  automatically. When unsure, use `Review`.
- **iCloud:** nothing inside iCloud may be trashed without `safety::ICloud::ConsentGiven`,
  which may only be constructed after explicit consent from the person.
- **Log what you do:** `info!` for user-visible actions, `debug!` for details, `warn!`
  when something is skipped or refused.
- Add a test for every new safety rule or judgement rule.

## Adding a cleanup rule

Rules live in `crates/sapu-core/src/judge.rs`, in priority order (Keep rules first).
Include in the PR: the path, which app creates it, how you confirmed it's safe, and a
test case in `judge.rs`.

## Project layout

See the "How it's built" section in the README.
