# Alder

*Alder* is a Rust prototype for plaintext, agent-friendly file routing: a small, explainable alternative to Hazel-style file sorting.

The initial target is sorting PDFs from `~/Downloads` into a document folder hierarchy using:

- Rust for a single dependable binary
- Watchman for efficient filesystem change detection
- CEL-like expressions for safe rule predicates
- YAML-first plaintext configuration
- Pluggable fact domains such as `file.*`, `pdf.*`, and `spotlight.*`
- Fresh fact extraction by default, without a mandatory persistent fact cache
- Dry-run, explain, action logging, and undo-friendly safety affordances

## Status

Alder can currently:

- parse strict YAML configs;
- evaluate simple CEL-like predicates;
- produce `file.*`, `pdf.text`, and `spotlight.*` facts;
- extract regex variables and render safe destination templates;
- dry-run and explain move plans;
- execute safe move actions inside explicit destination roots;
- execute OS-specific trash actions using the user's Trash/Recycle Bin;
- handle conflicts including `append_counter` and `replace_if_same_hash`;
- append action-log records;
- conservatively undo the last move, and restore exactly identified trash actions by action ID where the platform exposes Trash/Recycle Bin inventory APIs;
- generate and sync Watchman triggers that invoke Alder directly;
- run e2e CLI tests for the main workflow.

The implementation is still a prototype. The expression engine is provisional, JSON output is not yet versioned, and only move and trash actions are executed.

## Documentation

Start with:

- [Walkthrough](docs/walkthrough.md) for a copy-pasteable local sandbox flow.
- [Config reference](docs/config-reference.md) for supported YAML keys.
- [Known limitations](docs/known-limitations.md) for current scope boundaries.
- [JSON output](docs/json-output.md) for current machine-readable shapes.

Design and background:

- [Design sketch](docs/design.md)
- [`organize` comparison](docs/comparison-organize.md)
- [Watchman integration](docs/watchman.md)
- [CEL implementation evaluation](docs/cel-evaluation.md)
- [Generic date extraction design](docs/date-extraction.md)
- [Semantic type assessment](docs/semantic-types.md)
- [End-to-end test plan](docs/e2e-tests.md)

## Module map

- `src/config.rs` — strict YAML schema and validation.
- `src/expr.rs` — provisional CEL-like evaluator.
- `src/facts/` — file, PDF, and Spotlight fact providers.
- `src/render.rs` — extraction and template rendering.
- `src/planning.rs` — rules to action plans.
- `src/execute.rs` — move execution, conflict handling, action logs, undo.
- `src/watchman.rs` — Watchman trigger generation and sync.
- `src/pipeline.rs` — CLI-facing orchestration.
- `src/main.rs` — CLI parsing and command dispatch.
