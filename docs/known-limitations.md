# Known limitations

This document separates intentional scope boundaries from planned gaps and external dependencies.

## By design for now

### Alder is not a general automation platform

The current implementation focuses on explainable move-based file routing. It intentionally does not provide arbitrary shell/Python actions, notifications, or broad workflow orchestration.

### Move is the only executed action

The config schema can parse other actions, but only `move` is planned and executed today. Unsupported actions fail during planning.

### Destination roots are required for execution

Non-dry-run moves require `defaults.destination_roots`. This is deliberate. Alder should not infer its safety boundary from a rendered destination path.

### Dedupe is not undoable

`replace_if_same_hash` removes the source only after proving the destination already has the same size and hash. The destination is never deleted or overwritten. Because this is duplicate-source removal, `alder undo` does not recreate deduped sources.

### Watchman is a candidate detector

Watchman filters are prefilters only. Alder re-applies include/ignore logic and filesystem safety checks internally.

## Known gaps planned for later

### Expression engine is provisional

`src/expr.rs` is a small CEL-like evaluator. It supports the current MVP rules but is not intended to grow into a full language. See [CEL implementation evaluation](cel-evaluation.md).

### Fact orchestration is provider-level

Alder avoids expensive providers unless their domains are referenced, but it is not yet a fully on-demand per-key fact store. A future fact store should invoke providers through `facts.get("domain.key")` and memoize results.

### Stabilization config is parsed but not fully enforced

`stabilize.unchanged_for` and `stabilize.timeout` are present in the schema, but the current CLI pipeline does not yet wait for file stability before processing.

### Undo supports the last move only

`alder undo` and `alder undo last` are implemented. Undo by action ID, file path, run ID, or time range is not yet implemented.

### Reconciliation is an API, not a CLI command

The execution layer can detect in-progress action-log records without terminal records, but there is no `alder reconcile` command yet.

### JSON output is not versioned yet

The current JSON output is useful for tests and local automation, but there is no `schema_version` envelope on CLI JSON responses yet. See [JSON output](json-output.md).

### `.watchmanconfig` is not managed

`alder watchman sync` registers Watchman watches and triggers through Watchman's JSON protocol. It does not write `.watchmanconfig`. If that is added later, it should be explicit and previewable.

## External dependencies

### PDF text extraction uses `pdftotext`

`pdf.text` comes from the `pdftotext` subprocess. There is no built-in PDF parser fallback in Alder yet.

### No OCR

Scanned PDFs are not OCRed. OCR should remain an optional provider because it is expensive and has more operational dependencies.

### Spotlight is macOS-specific

`spotlight.*` facts use `mdls` and are unavailable on non-macOS platforms. Spotlight failures are reported as provider errors rather than aborting the entire batch.

### Watchman is required for automatic watching

The automatic watch path currently relies on Watchman. Manual `run` and `ingest` do not require Watchman.
