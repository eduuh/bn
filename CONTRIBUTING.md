# Contributing

Issues and pull requests are welcome. For anything larger than a small fix, open an issue first so
we can agree on the shape before you write it.

## Build and test

```sh
./build.sh                                   # build and install bn from this checkout
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three checks plus the script gates under `scripts/`. A pull request from a fork
runs on GitHub-hosted runners.

## Conventions

- One logical change per commit, with a conventional message (`feat:`, `fix:`, `chore:`, `docs:`).
- Match the surrounding code: its naming, comment density and error handling.
- Skills live in `config/prompts/<name>.md`. Every description reads "what it does. Use when ...".
- Keep machine-specific or private material out of the repo: personal skills and jobs belong in
  your notes tree or `~/.config/bn/`, which bn already reads.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
