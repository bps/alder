# Comparison with organize

[`organize`](https://github.com/tfeldmann/organize) is Alder's closest prior art: a mature, cross-platform, Python-based command-line alternative to Hazel and File Juggler.

The main lesson is that Alder should *not* try to out-feature `organize` broadly. Alder should stay narrower: a Rust-native, safer-by-default, explainable document router with strong dry-run, explain, and action-log semantics.

## Ideas to borrow first

These are the parts of `organize` that should most directly shape Alder's MVP and tests.

### Clear simulate/run split

`organize` has a simple operational distinction:

```sh
organize sim
organize run
```

Alder should preserve this clarity even if the command spelling is different:

```sh
alder run ~/Downloads --dry-run
alder run ~/Downloads
```

Dry-run output should be deterministic and reviewable, with a JSON mode suitable for agents.

### Practical conflict policies

`organize` has well-tested conflict behavior for move/copy actions, including skip, overwrite, and rename-new-style policies.

Alder should borrow the practical shape while keeping safer defaults:

- `error`
- `skip`
- `append_counter` / rename-new equivalent
- `replace_if_same_hash`
- `review`

Conflict behavior should be tested as a first-class safety feature, not treated as incidental filesystem plumbing.

### Named regex extraction from file content

`organize`'s `filecontent` filter can match document text and expose named regex groups as placeholders.

Alder should support the same workflow through explicit facts and extractors:

```yaml
when: contains(pdf.text, "American Express")
extract:
  statement_date:
    from: pdf.text
    regex: "Closing Date\\s+(?P<statement_date>\\d{2}/\\d{2}/\\d{4})"
```

This is especially important for PDF statements, invoices, receipts, and other semi-structured documents.

### Template ergonomics

`organize` templates are powerful and convenient. Alder should aim for similarly pleasant destination templates, while enforcing stricter safety rules around rendered paths.

Borrow:

- readable placeholders;
- date formatting;
- access to extracted values;
- access to core file facts.

Keep Alder-specific safeguards:

- reject or sanitize untrusted path segments intentionally;
- reject destination traversal;
- reject unknown template variables;
- plan actions before executing them.

### JSONL or structured output

`organize` has JSONL output events for machine consumption.

Alder should make structured output central for:

- `facts --json`;
- `explain --json`;
- `run --dry-run --json`;
- action logs.

Unlike general command output, Alder's action log should be append-only historical state and should include enough information for audit and conservative undo.

### Broad fixture coverage

`organize` has extensive tests for filters, actions, conflict handling, and combined behavior.

Alder should copy the testing discipline, especially around:

- config parsing and unknown-key rejection;
- file fact extraction;
- PDF text extraction failures;
- regex extraction;
- destination rendering safety;
- move conflict policies;
- dry-run action plans;
- explain output stability.

## Key differences

| Area | organize | Alder target |
| --- | --- | --- |
| Maturity | Production/stable | Prototype/design-stage |
| Runtime | Python package | Rust binary |
| Scope | General file automation | Narrow document/file routing engine |
| Rule model | Locations, filters, actions | Facts, expressions, extractors, action plans |
| Extensibility | Python filters/actions, shell actions | Safe providers/actions/plugins, likely subprocess boundary for heavyweight providers |
| Expression safety | Maximum flexibility, including Python/shell escape hatches | Sandboxed CEL or CEL-like predicates with no direct I/O |
| Explainability | Logs and JSONL output | First-class `facts`, `explain`, and dry-run plans |
| State | Execution output and filesystem effects | Append-only action log plus conservative undo model |
| Watching | Manual or externally scheduled runs | Watchman-first ingestion/watching design |

## Where organize is ahead

`organize` already provides many features Alder has only designed or prototyped:

- mature YAML/JSON config;
- simulation mode;
- move/copy/rename/delete/trash/symlink/hardlink/write/echo/confirm/macOS tag actions;
- many filters, including extension, name, regex, size, dates, duplicates, EXIF, MIME type, macOS tags, and file content;
- PDF text extraction via `pdftotext` with `pdfminer` fallback;
- DOCX/text/log content extraction;
- rich templating;
- cross-platform support;
- JSONL output;
- extensive documentation and tests.

For a user who needs a working Hazel-style command-line sorter today, `organize` is the better choice.

## Why Alder can still be distinct

Alder is justified only if it stays focused on capabilities that are not the center of `organize`:

- minimal-friction Rust binary distribution;
- strict config parsing by default;
- first-class fact domains such as `file.*`, `pdf.*`, and `spotlight.*`;
- safe, deterministic expressions rather than arbitrary Python or shell in normal rules;
- explicit action planning before execution;
- stable machine-readable `facts`, `explain`, and dry-run output;
- append-only action logs with enough information for audit and conservative undo;
- Watchman integration for efficient candidate detection;
- agent-friendly behavior with stable IDs, predictable outputs, and fixture-based tests.

## Scope guidance

Alder should avoid a broad feature race with `organize`.

Do borrow proven workflow ideas. Do not copy the entire automation-platform surface. The near-term target should be:

> a smaller, stricter, Rust-native, explainable document router with excellent dry-run, explain, and action-log semantics.
