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

The result can be transformed into an `alder ingest` call once the CLI is wired to the planner:

```sh
alder ingest ~/Downloads/foo.pdf ~/Downloads/bar.pdf --dry-run
```

Until then, `alder run ~/Downloads --dry-run` remains the intended human-facing command shape.

## Alder-managed triggers

The preferred workflow is for Alder to generate and synchronize Watchman trigger definitions directly from `alder.yaml`, avoiding a shell wrapper.

Expected commands:

```sh
# Print generated trigger JSON without changing Watchman state.
alder watchman print --config ~/src/alder/tmp/alder.yaml

# Register or update Alder-owned watches and triggers.
alder watchman sync --config ~/src/alder/tmp/alder.yaml

# Verify that Watchman state matches config-derived trigger definitions.
alder watchman check --config ~/src/alder/tmp/alder.yaml

# Remove Alder-owned triggers.
alder watchman unsync --config ~/src/alder/tmp/alder.yaml
```

For a watch path such as `~/src/alder/tmp/inbox`, Alder should register an extended Watchman trigger roughly like this:

```json
[
  "trigger",
  "/Users/example/src/alder/tmp/inbox",
  {
    "name": "alder",
    "expression": [
      "allof",
      ["type", "f"],
      ["suffix", "pdf"],
      [
        "not",
        [
          "anyof",
          ["match", "*.download", "wholename"],
          ["match", "*.crdownload", "wholename"],
          ["match", "*.part", "wholename"],
          ["match", "*.tmp", "wholename"]
        ]
      ]
    ],
    "command": [
      "/path/to/alder",
      "ingest",
      "--from-watchman",
      "--config",
      "/Users/example/src/alder/tmp/alder.yaml"
    ],
    "append_files": false,
    "stdin": ["name", "exists", "type"]
  }
]
```

This avoids shell expansion and command-line length issues. Watchman sends structured JSON on stdin, and Alder resolves each relative `name` using `WATCHMAN_ROOT`.

`alder ingest --from-watchman` should:

- read Watchman JSON from stdin;
- require `WATCHMAN_ROOT`;
- convert relative names to absolute paths;
- discard deleted entries and non-files;
- re-apply Alder's own include and ignore rules;
- stabilize candidates;
- run the normal ingest pipeline.

If `watch.ignore` changes in `alder.yaml`, a later `alder watchman sync` should update the Watchman trigger expression so the Watchman prefilter stays in sync with Alder's internal filtering.

## Wrapper fallback

A shell wrapper remains a useful fallback for early experiments or unusual environments, but it should not be required for the normal workflow.

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
2. Alder-managed Watchman triggers invoke `alder ingest --from-watchman` directly.
3. Alder stabilizes candidates.
4. Alder produces facts and evaluates rules.
5. Alder produces an action plan.
6. Alder dry-runs or executes the plan.
7. Alder appends action-log records for executed moves.

This keeps Watchman replaceable. A later polling or native watcher frontend should be able to feed the same `ingest PATH...` command without changing rule semantics.
