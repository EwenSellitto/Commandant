# Commandant

- **Checks**: the commands under "Development" in `README.md`. The pre-commit hook runs formatting and clippy; CI runs the tests. GUI tests live in the `commandant` binary, not a library: run a subset with `cargo test -p commandant-client --features gui-tests --bin commandant gui::<module>`.
- **File map**: `docs/crates.md` says what every file does. Read it to find code; when a change alters what a file does, update that file's row in the same commit.
- **Design**: `docs/architecture.md` explains how the server, workers and clients fit together.
