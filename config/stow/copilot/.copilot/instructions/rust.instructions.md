---
applyTo: "**/*.rs"
---

# Rust in bn

- Run `cargo fmt` and `cargo clippy` before committing; keep `cargo test` green.
- Prefer small functions and `?` over `unwrap` in non-test code.
- Branch-note and stow behavior changes need a test next to the code they change.
