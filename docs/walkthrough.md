# Walkthrough

This walkthrough exercises the current working Alder path in a local sandbox. It does not touch `~/Downloads` or real document folders.

Run these commands from the repository root.

## Build

```sh
cargo build
```

## Create a sandbox

```sh
rm -rf tmp
mkdir -p tmp/inbox tmp/sorted tmp/review tmp/state
```

## Write a config

Create `tmp/alder.yaml`:

```yaml
version: 1

watch:
  paths:
    - ./tmp/inbox
  include:
    - "*.pdf"
  ignore:
    - "*.tmp"
    - "*.download"
    - "*.crdownload"
    - "*.part"

defaults:
  conflict: append_counter
  destination_roots:
    - ./tmp/sorted

rules:
  - id: pdfs
    name: PDFs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "./tmp/sorted/{{ file.name }}"
```

`defaults.destination_roots` is required for non-dry-run move execution. It is the filesystem safety boundary for moves; `trash` actions use the operating system Trash/Recycle Bin and do not need a destination root.

## Dry-run a file

```sh
echo "fake pdf" > tmp/inbox/statement.pdf
cargo run -- --config tmp/alder.yaml run tmp/inbox --dry-run
```

Expected shape:

```text
File: tmp/inbox/statement.pdf
  Rule pdfs: matched
  Plan: pdfs
    Move { ... }
  Executed move: Planned -> .../tmp/sorted/statement.pdf
```

The source should still exist and the destination should not:

```sh
ls tmp/inbox tmp/sorted
```

## Execute the move

```sh
cargo run -- --config tmp/alder.yaml run tmp/inbox
```

Expected shape:

```text
File: tmp/inbox/statement.pdf
  Rule pdfs: matched
  Plan: pdfs
    Move { ... }
  Executed move: Moved -> .../tmp/sorted/statement.pdf
```

Verify:

```sh
ls tmp/inbox tmp/sorted
```

`statement.pdf` should now be under `tmp/sorted`.

## Inspect facts

```sh
cargo run -- --config tmp/alder.yaml --json facts tmp/sorted/statement.pdf
```

This prints `file.*` facts and provider reports. Because the rule does not reference `pdf.text`, the PDF provider should be reported as `not_required`.

## Explain a decision

Move the file back for another dry-run:

```sh
mv tmp/sorted/statement.pdf tmp/inbox/statement.pdf
cargo run -- --config tmp/alder.yaml --json explain tmp/inbox/statement.pdf
```

The JSON includes:

- produced facts;
- provider reports;
- rule evaluations;
- extracted variables, if any;
- the planned action.

## Undo the last move

Execute again, then undo:

```sh
cargo run -- --config tmp/alder.yaml run tmp/inbox
cargo run -- --config tmp/alder.yaml undo
```

Expected shape:

```text
Undid move: .../tmp/sorted/statement.pdf -> .../tmp/inbox/statement.pdf
```

Undo verifies that the destination still matches the logged hash and size before restoring it.

## Use Watchman without a shell wrapper

Generate the trigger definition:

```sh
cargo run -- --config tmp/alder.yaml watchman print
```

Install it:

```sh
cargo run -- --config tmp/alder.yaml watchman sync
cargo run -- --config tmp/alder.yaml watchman check
```

Create a candidate:

```sh
echo "fake pdf" > tmp/inbox/watchman.pdf
```

Watchman invokes Alder directly as:

```text
alder ingest --from-watchman --config ...
```

After Watchman processes the event, `watchman.pdf` should move to `tmp/sorted`.

To inspect Watchman logs on this machine:

```sh
watchman get-log
# then tail the path returned in the `log` field
```

Remove the trigger when finished:

```sh
cargo run -- --config tmp/alder.yaml watchman unsync
watchman watch-del "$(pwd)/tmp/inbox"
```

## JSON mode

Most operational commands accept global `--json`:

```sh
cargo run -- --config tmp/alder.yaml --json run tmp/inbox --dry-run
cargo run -- --config tmp/alder.yaml --json facts tmp/inbox/statement.pdf
cargo run -- --config tmp/alder.yaml --json explain tmp/inbox/statement.pdf
cargo run -- --config tmp/alder.yaml --json undo
```

The current JSON shapes are documented in [JSON output](json-output.md).
