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

See [`docs/design.md`](docs/design.md) for the current design sketch, tradeoffs, and open questions.

## Status

Design only. No implementation has been started yet.
