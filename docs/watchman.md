# Watchman integration

Alder treats Watchman as a candidate detector, not as the rule engine or durable state store.

Watchman should answer:

- which watched paths changed;
- which paths are likely candidates, such as PDFs in `~/Downloads`;
- when to batch or debounce noisy filesystem events.

Alder should answer:

- whether a candidate file is stable enough to process;
- which facts are needed;
- which rules match;
- which action plan is safe;
- what was actually executed and logged.

## Manual query workflow

For early development, use Watchman to list candidate files and pass them to Alder explicitly.

```sh
watchman watch-project ~/Downloads

watchman query ~/Downloads --since c:0 --fields name type exists --expression '["allof", ["type", "f"], ["suffix", "pdf"]]'
```

The result can be transformed into an `alder ingest` call by a small wrapper script once the CLI is wired to the planner:

```sh
alder ingest ~/Downloads/foo.pdf ~/Downloads/bar.pdf --dry-run
```

Until then, `alder run ~/Downloads --dry-run` remains the intended human-facing command shape.

## Trigger sketch

A Watchman trigger can invoke Alder for changed PDFs under `~/Downloads`:

```sh
watchman -- trigger ~/Downloads alder-pdf-sort '**/*.pdf' -- alder ingest --dry-run
```

A production trigger should prefer a wrapper script so it can:

- receive the Watchman file list on stdin;
- convert relative names to absolute paths;
- drop deleted files and directories;
- ignore temporary download suffixes;
- batch paths into one Alder invocation;
- capture stdout/stderr for debugging.

Example wrapper shape:

```bash
#!/usr/bin/env bash
set -euo pipefail

root=${WATCHMAN_ROOT:?}
mapfile -t files

candidates=()
for rel in "${files[@]}"; do
  case "$rel" in
    *.download|*.crdownload|*.part|*.tmp) continue ;;
    *.pdf) candidates+=("$root/$rel") ;;
  esac
done

if ((${#candidates[@]})); then
  alder ingest "${candidates[@]}" --dry-run
fi
```

## Candidate filtering

Use Watchman for cheap prefilters only:

- file type is regular file;
- suffix is `.pdf`;
- path is under the inbox;
- obvious temporary download suffixes are excluded.

Do not encode document semantics in Watchman expressions. Rules such as “American Express statement” belong in Alder, where they can be explained, tested, and logged.

## Stabilization

Watchman settling reduces event noise but does not prove a file is complete.

Alder still needs its own settled-file predicate before execution:

- path still exists;
- path is a regular non-symlink file;
- name does not match temporary suffixes;
- size and modified time are unchanged for the configured interval;
- file can be opened for reading;
- optional lightweight PDF validation passes or routes to review.

The Watchman trigger should be allowed to over-report. Alder should be responsible for refusing unsafe or unstable candidates.

## Idempotency and concurrency

Alder's simplest idempotency model is structural:

- watch the inbox, not destination folders;
- move successfully processed files out of the inbox;
- use deterministic destinations;
- enforce explicit conflict policies;
- append action-log records for executed moves.

If Watchman reports the same file multiple times, Alder should avoid concurrent mutation with execution/action-log locking. The MVP executor already uses an exclusive lock on the action log during execution; a future watcher wrapper should avoid launching overlapping Alder processes for the same inbox when possible.

## Recommended MVP responsibilities

For the MVP, keep the boundary simple:

1. Watchman watches `~/Downloads` and reports candidate paths.
2. A wrapper invokes `alder ingest PATH... --dry-run` or `alder ingest PATH...`.
3. Alder stabilizes candidates.
4. Alder produces facts and evaluates rules.
5. Alder produces an action plan.
6. Alder dry-runs or executes the plan.
7. Alder appends action-log records for executed moves.

This keeps Watchman replaceable. A later polling or native watcher frontend should be able to feed the same `ingest PATH...` command without changing rule semantics.
