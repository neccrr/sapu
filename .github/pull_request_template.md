## What does this change?

## Checklist
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Anything that deletes files goes through `cleaner` + `safety` + the journal (never `fs::remove_*`)
- [ ] New cleanup rules: tested on a real Mac, and the verdict is honest (Safe only if it's rebuilt automatically)
