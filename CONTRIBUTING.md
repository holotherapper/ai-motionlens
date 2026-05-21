# Contributing

Thank you for your interest in contributing to ai-motionlens.

## Development

```sh
# Build the whole workspace
cargo build --workspace

# Run unit / doc tests
cargo test --workspace

# Lint (must be warning-free)
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all
```

### E2E (browser-driven) tests

The full E2E harness drives a real Chrome via CDP. It lives in `e2e/`.

```sh
# One-shot: build release MCP and run every test
./e2e/run.sh

# Single test
./e2e/run.sh test_motion_verify
```

If you don't already have a usable Chrome on PATH, run the helper to install
Chrome-for-Testing into `~/.cache/ai-motionlens/chrome-for-testing/` and have
it print the `CHROME_PATH` you should export:

```sh
eval "$(./scripts/install-chrome-for-testing.sh --eval)"
```

The E2E harness uses Python via `uv run --no-project`, so it does not require
any virtualenv setup.

## Pull Requests

1. Fork the repository.
2. Create a feature branch (`git checkout -b feature/my-feature`).
3. Ensure `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` pass.
4. If the change touches MCP tool surface or the verifier, also run the relevant E2E test(s) under `e2e/tests/`.
5. Commit with a [gitmoji](https://gitmoji.dev/) prefix (e.g. `✨ Add new feature`).
6. Open a pull request describing the motion-verification behaviour change (what intent / contract is now passing or failing differently).

## Reporting Issues

Please open an issue with:
- Steps to reproduce (URL or minimal HTML fixture)
- `MotionContract` used (or CLI flags)
- Expected vs actual `MotionVerifyReport` (especially `passes`, `verdict`, `diagnosis_hints`)
- `ai-motionlens --version` output
- OS and architecture
- Chrome / Chromium version

## License

By contributing, you agree that your contributions will be dual-licensed
under the MIT license and the Apache License 2.0, at the user's option,
matching the project's license.
