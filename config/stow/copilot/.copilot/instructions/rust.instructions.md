---
applyTo: "**/*.rs"
---

# Rust in Workspan

- Run `cargo fmt` and `cargo clippy` before committing; keep `cargo test` green.
- Prefer small functions and `?` over `unwrap` in non-test code.
- Task-state, execution-state, and stow behavior changes need a test next to the code they change.
