# Alder

*Alder* is a design-stage project for a plaintext, agent-friendly replacement for Hazel-style file sorting.

The initial target is sorting PDFs from `~/Downloads` into a document folder hierarchy using:

- Rust for a single dependable binary
- Watchman for efficient filesystem change detection
- CEL-like expressions for safe rule predicates
- YAML-first plaintext configuration
- Pluggable fact domains such as `file.*`, `pdf.*`, and `spotlight.*`
- Fresh fact extraction by default, without a mandatory persistent fact cache
- Dry-run, explain, action logging, and undo-friendly safety affordances

See [`docs/design.md`](docs/design.md) for the current design sketch, tradeoffs, and open questions. Additional notes cover the [`organize` comparison](docs/comparison-organize.md) and [Watchman integration](docs/watchman.md).

## Status

A Rust CLI skeleton has been started. Commands currently validate arguments and return explicit "not yet implemented" responses while the MVP pieces are built out.

Implemented library pieces include strict YAML config parsing, `file.*` facts, PDF text extraction via `pdftotext`, Spotlight metadata via `mdls`, and a provisional CEL-like expression evaluator.
