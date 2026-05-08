# End-to-end test plan

Alder needs integration tests that exercise the CLI the way a user or Watchman trigger would.

The current unit tests cover most modules, but the shell-facing behavior is now important enough to test directly.

## Proposed test harness

Use Rust integration tests in `tests/e2e.rs`.

Each test should:

- use `env!("CARGO_BIN_EXE_alder")` to locate the compiled binary;
- create a `tempfile::TempDir` sandbox;
- set `HOME` to a temp home directory so action logs stay inside the sandbox;
- write a complete `alder.yaml` with absolute temp paths;
- invoke the CLI with `std::process::Command`;
- assert on exit status, filesystem effects, JSON output, and action-log contents.

Avoid real Watchman server state in normal e2e tests. Use `watchman print` and `ingest --from-watchman` for deterministic coverage. Real `watchman sync/check` can remain a manual or ignored integration test because it depends on a local Watchman daemon.

## Opt-in real OS trash tests

The `real_os_trash_run_and_undo_by_action_id` integration test exercises Alder's `trash` action through the real operating-system Trash/Recycle Bin. It is ignored by default and also requires an explicit environment variable so routine test runs and broad `cargo test -- --ignored` runs do not unexpectedly touch the user's Trash.

Run it only on supported Linux/Freedesktop-style Unix or Windows hosts:

```sh
ALDER_RUN_REAL_OS_TRASH_TESTS=1 cargo test --test e2e real_os_trash_run_and_undo_by_action_id -- --ignored --nocapture
```

The test creates a temporary source file named `alder-real-os-trash-test-<uuid>.txt`, runs the CLI so Alder calls `trash::delete`, verifies the action log restore metadata, and then runs `alder undo <action_id>` to restore the exact trash item through `trash::os_limited`. After restore, the temp directory cleanup removes the test file. On Windows, a test-only guard also attempts to purge only the exact original-path trash item if the test fails before undo and the item is visible through the current platform trash inventory.

Safety notes:

- the test keeps the source file and sandboxed `HOME` under the same temporary root, and sets `HOME`, `XDG_CONFIG_HOME`, and `XDG_DATA_HOME` for the Alder subprocess so logs and Freedesktop trash state use the sandbox where the platform honors those variables;
- Windows Recycle Bin behavior is system-managed, so the test still uses a unique temp path and Alder's exact-action restore metadata;
- if the process is interrupted or the platform ignores sandboxed trash locations, manually remove any `alder-real-os-trash-test-<uuid>.txt` item whose original path points inside the test temp directory;
- macOS is intentionally not covered by this real-trash test because the `trash` crate does not expose the `os_limited` inventory/restore APIs Alder uses for automated cleanup there.

## Initial e2e cases

### Dry-run does not move files

Command:

```sh
alder --config <tmp>/alder.yaml --json run <tmp>/inbox --dry-run
```

Assertions:

- exits 0;
- stdout is valid JSON;
- result includes a plan;
- execution record status is `planned`;
- source file still exists;
- destination file does not exist.

### Run moves and logs

Command:

```sh
alder --config <tmp>/alder.yaml --json run <tmp>/inbox
```

Assertions:

- exits 0;
- source file is gone;
- destination file exists with same contents;
- action log exists under temp `HOME/.local/state/alder/actions.jsonl`;
- log contains `in_progress` and `moved` records.

### Facts JSON exposes cheap facts and provider reports

Command:

```sh
alder --config <tmp>/alder.yaml --json facts <tmp>/inbox/file.pdf
```

Assertions:

- exits 0;
- output includes `file.name` and `file.ext`;
- provider reports include `file` as invoked;
- provider reports include `pdf` as `not_required` for configs that do not reference `pdf.text`.

### Explain JSON includes matched rule and planned destination

Command:

```sh
alder --config <tmp>/alder.yaml --json explain <tmp>/inbox/file.pdf
```

Assertions:

- exits 0;
- rule evaluation for the configured rule is matched;
- plan includes a move action;
- move destination points into the temp sorted directory.

### Watchman print generates direct Alder trigger

Command:

```sh
alder --config <tmp>/alder.yaml watchman print
```

Assertions:

- exits 0;
- output is valid JSON;
- command includes `ingest --from-watchman`;
- `append_files` is false;
- `stdin` is `["name", "exists", "type"]`;
- expression reflects config include/ignore lists.

### Ingest from Watchman moves candidates

Command stdin:

```json
[
  {"name":"statement.pdf","exists":true,"type":"f"},
  {"name":"ignored.pdf.tmp","exists":true,"type":"f"},
  {"name":"deleted.pdf","exists":false,"type":"f"},
  {"name":"folder.pdf","exists":true,"type":"d"}
]
```

Command:

```sh
WATCHMAN_ROOT=<tmp>/inbox alder --config <tmp>/alder.yaml --json ingest --from-watchman
```

Assertions:

- exits 0;
- only `statement.pdf` is moved;
- ignored/deleted/non-file entries are not moved or planned;
- output includes one result.

## Later e2e cases

- PDF text rule using a small fixture or fake provider.
- Date extraction and template formatting.
- Conflict handling with `append_counter`.
- Destination root rejection.
- Watchman sync/check against a real Watchman daemon, marked ignored or run only when Watchman is available.
