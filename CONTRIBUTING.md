# Contributing

Alder is a prototype, so small, focused changes are easiest to review.

## Development commands

Run formatting before submitting changes:

```sh
cargo fmt
```

Run Clippy with the same settings used by CI:

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

Run the normal test suite:

```sh
cargo test --all-features --locked
```

## Opt-in tests

The real operating-system Trash/Recycle Bin integration test is ignored by
default and also requires an explicit environment variable. Run it only when you
intend to exercise the host Trash/Recycle Bin behavior:

```sh
ALDER_RUN_REAL_OS_TRASH_TESTS=1 cargo test --test e2e real_os_trash_run_and_undo_by_action_id -- --ignored --nocapture
```

See [docs/e2e-tests.md](docs/e2e-tests.md) for details and safety notes.
