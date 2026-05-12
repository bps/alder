# Known limitations

This document separates intentional scope boundaries from planned gaps and external dependencies.

## By design for now

### Alder is not a general automation platform

The current implementation focuses on explainable file routing. It intentionally does not provide arbitrary shell/Python actions or broad workflow orchestration. On macOS, CLI execution emits best-effort status notifications via `osascript`; these are informational notifications without custom action buttons.

### Only a small action set executes

The config schema can parse several action placeholders, but only `move`, `trash`, and non-destructive `scan_app_supporting_files` are planned and executed today. Unsupported actions fail during planning.

### Destination roots are required for execution

Non-dry-run moves require `defaults.destination_roots`. This is deliberate. Alder should not infer its safety boundary from a rendered destination path.

### Dedupe is not undoable

`replace_if_same_hash` removes the source only after proving the destination already has the same size and hash. The destination is never deleted or overwritten. Because this is duplicate-source removal, `alder undo` does not recreate deduped sources.

### Trash undo

`trash` actions use the operating system Trash/Recycle Bin and are intended to be restored there. `alder undo` and `alder undo last` still do not automatically restore the latest trash action; they refuse rather than guessing or reaching past it to undo an older move.

On Linux/Freedesktop and Windows, new trash records include conservative restore metadata when Alder can identify exactly one matching Trash/Recycle Bin item immediately after the trash operation. `alder undo <action_id>` can restore such a trash action only when the source path is still absent and the current Trash/Recycle Bin contains exactly one item matching the original path, deletion time, and size. Missing metadata, duplicate matches, missing matches, or source-path collisions all refuse automatic restore.

macOS does not expose the `trash::os_limited` inventory/restore APIs that Alder uses for conservative restore. On macOS, restore trash actions from Finder/Trash Put Back; `alder undo <action_id>` refuses automatic trash restore.

Trash behavior depends on the host OS and user environment. Headless/minimal Linux environments need a writable FreeDesktop-style trash location; removable, network, or unusual filesystems may fail or have platform-specific Recycle Bin behavior; and macOS protected locations may require appropriate user permissions. Alder reports the platform trash error and leaves the source in place when the trash operation fails.

If Alder exits after logging a trash intent but before logging success or failure, reconciliation can report the orphaned in-progress record but cannot always prove whether the OS completed the trash operation. Check the source path and the OS Trash/Recycle Bin manually in that case.

### Watchman is a candidate detector

Watchman filters are prefilters only. Alder re-applies include/ignore logic and filesystem safety checks internally.

## Known gaps planned for later

### Expression engine is provisional

`src/expr.rs` is a small CEL-like evaluator. It supports the current MVP rules but is not intended to grow into a full language. See [CEL implementation evaluation](cel-evaluation.md).

### Fact orchestration is provider-level

Alder avoids expensive providers unless their domains are referenced, but it is not yet a fully on-demand per-key fact store. A future fact store should invoke providers through `facts.get("domain.key")` and memoize results.

### Stabilization config is parsed but not fully enforced

`stabilize.unchanged_for` and `stabilize.timeout` are present in the schema, but the current CLI pipeline does not yet wait for file stability before processing.

### Undo selectors are limited

`alder undo` and `alder undo last` undo the latest move only. `alder undo <action_id>` accepts action ID UUIDs and is implemented for conservatively identified trash actions on platforms with Trash/Recycle Bin inventory APIs. Undo by file path, run ID, or time range is not yet implemented.

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
