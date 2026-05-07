# Alder design

## Summary

Alder is a proposed plaintext, agent-friendly replacement for Hazel-style file sorting.

The first use case is sorting PDFs from `~/Downloads` into a personal document hierarchy. The broader shape is a cross-platform file routing engine:

```text
files produce facts
rules query facts
actions transform files
every decision is explainable and reversible where practical
```

Alder should feel like a small dependable command-line tool, not a general workflow platform.

[`organize`](https://github.com/tfeldmann/organize) is the closest prior art and should be treated as a benchmark. Alder should borrow its practical strengths — clear simulate/run behavior, conflict policy ergonomics, named regex extraction from file content, template usability, structured output, and broad fixture coverage — while staying narrower, stricter, and more explainable. See [Comparison with organize](comparison-organize.md).

Alder should continue moving toward stronger semantic types for facts, paths, and template values as the prototype matures. See [Semantic type assessment](semantic-types.md).

## Goals

- Provide a Rust-based file sorting tool with minimal runtime/environment friction.
- Keep rules in plaintext so humans and agents can review, edit, test, and version them.
- Support safe expressions, likely CEL or a CEL-like language, for rule predicates.
- Integrate with Watchman for efficient filesystem change detection.
- Expose facts through explicit domains such as `file.*`, `pdf.*`, and `spotlight.*`.
- Prefer fresh fact extraction over a mandatory persistent fact cache.
- Provide strong safety affordances: dry-run, explain, action planning, conflict handling, action logs, and undo-friendly records.
- Keep platform-specific integrations as plugins or fact providers, not as assumptions in the core rule model.

## Non-goals

- Not a full Hazel clone.
- Not a general-purpose workflow engine.
- Not a backup, sync, or archival integrity system.
- Not initially a GUI application.
- Not initially a long-running cross-platform watcher abstraction. Watchman is the first watcher target.
- Not initially an OCR or LLM-first document classifier.

## Security model

Alder acts on files from places like `~/Downloads`, so filenames, PDF metadata, PDF text, and extracted values must be treated as untrusted input.

The action layer should enforce safety rules regardless of what a rule or template renders:

- Rendered destination paths must be normalized and must not escape configured destination roots unless explicitly allowed.
- Extracted strings used in paths must be sanitized or encoded.
- Symlinks should not be followed for destructive operations by default.
- Existing files should never be overwritten without an explicit conflict policy.
- Actions should operate on regular files by default, not directories, sockets, devices, or FIFOs.
- Provider subprocesses should receive paths as arguments, not through shell interpolation.
- Rules should not be able to perform I/O directly through the expression language.

## What agent-friendly means

Alder should be friendly to coding agents in concrete ways:

- Rules live in normal text files.
- Rule changes can be reviewed in diffs.
- `dry-run` produces deterministic planned actions.
- `explain` shows which facts were read, which rules matched, and why.
- Machine-readable output is available for `facts`, `explain`, and `dry-run`.
- Rules have stable names or IDs suitable for tests and logs.
- Fixtures can assert expected rule matches and destinations.
- The action log is structured and append-only.

Operationally, agent-facing commands should have stable stdout formats, JSON output modes, documented exit codes, and no hidden prompts unless an interactive flag is explicitly supplied.

## Core pipeline

```text
Watchman event batch
        ↓
alder ingest PATH...
        ↓
filter obvious non-candidates
        ↓
wait until each file is stable
        ↓
produce facts lazily from providers
        ↓
evaluate rules against facts
        ↓
run extractors and render templates
        ↓
produce an action plan
        ↓
dry-run or execute
        ↓
append action log
```

The watcher detects candidates. Alder owns the semantics.

## CLI sketch

```sh
alder run ~/Downloads --dry-run
alder ingest ~/Downloads/foo.pdf ~/Downloads/bar.pdf
alder watch
alder watchman print
alder watchman sync
alder watchman check
alder watchman unsync
alder facts ~/Downloads/foo.pdf
alder explain ~/Downloads/foo.pdf
alder test
alder undo last
```

Potential machine-readable forms:

```sh
alder explain ~/Downloads/foo.pdf --json
alder run ~/Downloads --dry-run --json
```

## Configuration

Alder should start with YAML because it is readable for nested rule/action structures and easy for agents to edit.

TOML can be supported later by deserializing into the same Rust model. The rule engine should not care which source format produced the config.

### Example

```yaml
version: 1

watch:
  paths:
    - ~/Downloads
  include:
    - "*.pdf"
  ignore:
    - "*.download"
    - "*.crdownload"
    - "*.part"
    - "*.tmp"
  settle: 5s

stabilize:
  unchanged_for: 3s
  timeout: 60s

defaults:
  conflict: append_counter
  unmatched:
    move_to: "~/Documents/_Inbox/PDF Review/{{ file.name }}"

rules:
  - id: amex-statements
    name: American Express statements
    when: |
      file.ext == ".pdf" &&
      (
        contains(pdf.text, "American Express") ||
        contains(spotlight.kMDItemTextContent, "American Express")
      ) &&
      contains(pdf.text, "Statement Closing Date")
    extract:
      statement_date:
        from: pdf.text
        regex: "Closing Date\\s+(\\d{2}/\\d{2}/\\d{4})"
        format: "%m/%d/%Y"
    actions:
      - move:
          to: "~/Documents/Finance/Credit Cards/Amex/{{ statement_date | date('%Y') }}/{{ statement_date }} - Amex.pdf"
```

A future first-class date extractor should reduce brittle per-issuer regexes for
converted Hazel custom date tokens while preserving this explicit regex path. See
[Generic date extraction design](date-extraction.md).

### Strict schema

The parser should reject unknown keys by default. Silent config acceptance is dangerous in a file-moving tool.

## Expression language

CEL is the leading candidate because it is designed for safe embedded policy evaluation.

Alder should validate the Rust CEL implementation before making CEL a hard architectural dependency. The current recommendation is to keep Alder's provisional evaluator behind an adapter boundary while prototyping the `cel` crate as the preferred replacement candidate. See [CEL implementation evaluation](cel-evaluation.md).

Evaluation criteria:

- Can host values be injected as nested objects such as `file.ext` and `pdf.text`?
- Can custom functions be added, e.g. `contains`, `matches`, `lower`, `date_parse`?
- Are errors clear enough for rule authors?
- Is evaluation deterministic and sandboxed?
- Can missing facts be handled gracefully?
- Does evaluation have no I/O capability?
- Can evaluation be bounded by time, recursion depth, or expression complexity?
- Is the grammar stable enough for long-lived rule files?

Alternatives if CEL support is not mature enough:

- Rhai
- Starlark via `starlark-rust`
- A small custom predicate language
- A query model backed by SQLite later

The design should avoid baking in syntax that prevents switching expression engines during the first prototype.

## Fact model

Rules query facts. Facts are namespaced by provider domain.

Example fact domains:

```text
file.path
file.name
file.stem
file.ext
file.size
file.created_at
file.modified_at
file.sha256

pdf.title
pdf.author
pdf.page_count
pdf.text
pdf.first_page_text
pdf.detected_dates

spotlight.kMDItemWhereFroms
spotlight.kMDItemTextContent
spotlight.kMDItemAuthors
spotlight.kMDItemFSLabel
```

Facts should be lazy where possible. If no rule references `pdf.text`, Alder should not extract PDF text.

### Provider cost classes

Fact providers should declare rough cost:

- `cheap`: file metadata, path parsing
- `moderate`: Spotlight metadata, PDF metadata
- `expensive`: full PDF text extraction
- `very_expensive`: OCR, LLM classification

The first implementation can evaluate simply, but the design should preserve the ability to avoid expensive providers unless rules require them.

Even without a persistent cache, Alder may cache facts within a single run or ingestion batch. Provider calls should also support per-file timeouts so one malformed PDF cannot block an entire watch batch.

## Plugins and providers

Open decision: plugin boundary.

Possible models:

### In-process Rust traits

Pros:

- Fast
- Strong typing
- Simple development inside one crate/workspace

Cons:

- Harder third-party extensibility
- Native library/linking/licensing concerns affect the main binary

### Subprocess JSON providers

Pros:

- Language-agnostic
- Good isolation
- Easier to wrap platform tools like `mdls`, `pdftotext`, `exiftool`, or `ocrmypdf`
- Avoids linking/licensing issues in the core binary

Cons:

- Slower
- More failure modes
- Requires stable JSON protocol

### Hybrid

Core providers are in-process. Optional or heavyweight providers use subprocesses.

This is likely the best long-term model.

## Watchman responsibilities

Watchman should provide:

- Efficient recursive watching of configured `watch.paths` directories
- Candidate prefiltering, e.g. PDFs under `~/Downloads`
- Event batching
- Settling/debounce where useful
- Clocks/cursors for changed-since queries

Watchman should not be treated as the rule engine or durable state store.

Watchman is the initial watcher integration, not necessarily the only possible frontend. Later versions may add a polling mode or a native watcher fallback for users who do not want to install Watchman. See [Watchman integration](watchman.md) for the MVP boundary and trigger sketch.

### Alder-managed Watchman sync

Alder should avoid requiring users to maintain a shell wrapper around Watchman triggers.

Instead, Alder should be able to generate and synchronize Watchman trigger definitions from the same `watch.include`, `watch.ignore`, and `watch.paths` config that Alder uses internally:

```sh
alder watchman print --config alder.yaml
alder watchman sync --config alder.yaml
alder watchman check --config alder.yaml
alder watchman unsync --config alder.yaml
```

The generated trigger should invoke Alder directly and pass changed files over structured stdin rather than through shell argument expansion:

```json
[
  "trigger",
  "/Users/example/Downloads",
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
      "/path/to/alder.yaml"
    ],
    "append_files": false,
    "stdin": ["name", "exists", "type"]
  }
]
```

`alder ingest --from-watchman` should:

- read Watchman JSON from stdin;
- use `WATCHMAN_ROOT` to resolve relative file names;
- drop deleted entries and non-files;
- re-apply Alder's include and ignore filters internally;
- stabilize remaining candidates;
- then run the normal ingest pipeline.

The Watchman expression is only a cheap prefilter. It is not a trusted security boundary. Alder must still enforce include/ignore rules, source safety, destination-root checks, conflict policy, and action logging.

Writing `.watchmanconfig` should be optional and explicit because it modifies user directories. The default `watchman sync` behavior should register or update Alder-owned triggers through Watchman's API without writing files into watched roots.

## Watchman limitations

Watchman does not fully solve:

- File stabilization
- Idempotency
- Action history
- Rule match history
- Destination conflict handling
- Undo

Watchman's `settle` can reduce noisy events, but Alder still needs its own settled-file predicate.

## Stabilization

Alder should not process files while they are still being downloaded or written.

A candidate file is settled when, within a timeout:

- It still exists.
- It is a regular file.
- Its name does not match ignored temporary patterns.
- Its size and modified time are unchanged for `stabilize.unchanged_for`.
- It can be opened for reading.
- For PDFs, a lightweight validation succeeds if enabled.

PDF validation should be conservative. Some valid PDFs are strange, so validation failure should generally route to review or produce a clear error rather than silently deleting or moving the file.

## State model

Alder should not require a persistent fact cache for the MVP.

There are three distinct kinds of state:

1. Fact cache: derived metadata/text from files.
2. Processing state: whether a file has already been considered.
3. Action log: what Alder actually did.

The MVP should avoid a persistent fact cache, minimize processing state, and keep an append-only action log.

### Why avoid a mandatory fact cache?

Fact caches go stale unless carefully invalidated. For the initial `~/Downloads` PDF sorter, the candidate set is expected to be small enough that fresh extraction is simpler and more trustworthy.

Expensive providers must be lazy so that no-cache does not imply extracting all facts for all files.

### Action log

The action log should be append-only JSONL, stored under a platform-appropriate state directory such as:

```text
~/.local/state/alder/actions.jsonl
```

Log records should include a schema version from the first implementation.

A move log entry should include enough information to audit and possibly undo the action:

```json
{"schema_version":1,"ts":"2026-05-05T12:00:01Z","run_id":"...","rule_id":"amex-statements","action":"move","from":"/Users/example/Downloads/a.pdf","to":"/Users/example/Documents/Finance/Amex/a.pdf","sha256":"abc123","size":12345}
```

Undo should treat the log as historical truth, not as proof that the filesystem is still in the same state. If the destination has been moved or edited since the action, undo should fail safely with an explanation.

Undo for non-move actions should be action-specific and conservative. If the previous state cannot be proven or reconstructed from the log, Alder should refuse automatic undo rather than guessing.

## Idempotency

The simplest idempotency strategy is structural:

- Watch the inbox, not the destinations.
- Move matched files out of the inbox.
- Use deterministic destinations.
- Handle conflicts explicitly.
- Use action logs for audit and optional duplicate detection.

Alder should not repeatedly process a file after a successful move because the source file no longer exists in the watched tree.

If Watchman reports the same path multiple times before a move completes, Alder should use an in-process or filesystem lock keyed by canonical path.

Alder also needs a single-writer model for action execution. A manual `alder run` and a background `alder watch` should not be able to mutate the same inbox concurrently without coordination. The likely MVP approach is a process-level lock file around action execution and action-log appends.

## Conflict handling

Destination conflicts must be explicit. Possible policies:

```yaml
conflict: error
conflict: skip
conflict: append_counter
conflict: replace_if_same_hash
conflict: review
```

A safe default is likely `append_counter` or `review`. `replace_if_same_hash` is useful but requires hashing both source and destination.

### Worked scenario

If two rules match `~/Downloads/statement.pdf` and both propose a terminal move, Alder should not execute both.

MVP behavior:

1. Rules are evaluated in order.
2. The first matching rule with terminal actions wins.
3. `explain` reports later matching rules as shadowed, if evaluated.
4. A future `continue: true` option may allow non-terminal actions such as tagging before a move.

## Actions

Initial action types:

- `move`
- `copy`
- `rename`
- `tag` on macOS, possibly via Finder tags/xattrs
- `review` or `move_to_review`

All actions should first be represented as an action plan. Execution consumes the plan.

A plan should include:

- Source path
- Matched rule
- Extracted variables
- Destination path
- Conflict policy
- Whether the action is terminal

## Explainability

`alder explain FILE` should show:

- Facts requested and produced
- Providers invoked
- Rule evaluation results
- Extracted variables
- Planned actions
- Conflict resolution result
- Reasons for no match or failure

Example:

```text
File: ~/Downloads/eStmt.pdf

Matched rule:
  amex-statements - American Express statements

Facts:
  file.ext = ".pdf"
  pdf.text contains "American Express" = true
  pdf.text contains "Statement Closing Date" = true

Extracted:
  statement_date = 2026-04-15

Plan:
  move ~/Downloads/eStmt.pdf
    -> ~/Documents/Finance/Credit Cards/Amex/2026/2026-04-15 - Amex.pdf
```

## Testing

Alder should support fixture-based tests.

Possible layout:

```text
fixtures/
  amex-2026-04.pdf
  fidelity-2026-q1.pdf
expected.yaml
```

Example expectations:

```yaml
amex-2026-04.pdf:
  rule_id: amex-statements
  action: move
  to: "~/Documents/Finance/Credit Cards/Amex/2026/2026-04-15 - Amex.pdf"
```

Testing should not require moving real files. It should evaluate facts, rules, extractions, and action plans.

## Rust implementation notes

Likely crates to evaluate:

- `clap` for CLI parsing
- `serde`, `serde_yaml`, and `toml` for configuration
- `tracing` for logs
- `camino` for UTF-8 paths where appropriate
- `regex` for extractors
- `minijinja` or `tera` for destination templates
- `notify` only if a native watcher fallback is pursued
- CEL crates such as `cel-interpreter`, subject to evaluation

External command integrations to consider for early prototypes:

- `mdls` for macOS Spotlight facts
- `pdftotext` for PDF text extraction
- `exiftool` for broad document metadata
- `ocrmypdf` for optional OCR workflows

Using subprocess providers early may keep the Rust core focused on orchestration and safety.

## Comparison

| Axis | Hazel | Alder target |
| --- | --- | --- |
| Rule editing | GUI | Plaintext |
| Agent editing | Difficult | First-class |
| Platform | macOS | Cross-platform core, platform plugins |
| Metadata | Spotlight-heavy | Fact domains, including Spotlight on macOS |
| Watcher | Built in | Watchman initially |
| Explainability | Limited | CLI and JSON explain output |
| Testing | Manual | Fixture-based rule tests |
| State | App-managed | Minimal required state plus action log |
| Extensibility | App features | Providers/actions/plugins |

## MVP

1. Rust CLI skeleton.
2. YAML config parser with strict schema.
3. `file.*` provider.
4. `pdf.text` provider via `pdftotext` subprocess.
5. `spotlight.*` provider via `mdls` on macOS.
6. CEL or CEL-like expression evaluation proof of concept.
7. `run --dry-run`.
8. `explain`.
9. `move` action with conflict policy.
10. Append-only JSONL action log.
11. Watchman trigger documentation or helper command.

## Later roadmap

- TOML config support.
- Native PDF extraction fallback.
- OCR provider.
- LLM classification provider for unmatched documents.
- Optional SQLite fact cache/index.
- Rich history queries.
- Native watcher fallback via `notify`.
- More action types, including tags and notifications.
- Rule packs or reusable config modules.

## Open questions

- Which Rust CEL implementation is mature enough?
- Should the expression language be committed to CEL before a prototype?
- What should the plugin process boundary be?
- Should YAML remain the primary format, or should TOML be first-class from day one?
- How lazy can fact extraction be with the chosen expression evaluator?
- What is the minimum useful JSON protocol for subprocess fact providers?
- What level of PDF validation should be enabled by default?
- What is the safest default conflict policy?
- Should action logs include hashes by default, given the cost on large files?
- How should macOS tags be represented cross-platform?
