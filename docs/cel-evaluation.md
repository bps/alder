# CEL implementation evaluation

Alder currently uses a small provisional CEL-like evaluator implemented in `src/expr.rs`. This keeps the MVP safe and dependency-light while preserving a seam for a real CEL engine.

The leading Rust candidate is [`cel`](https://github.com/cel-rust/cel-rust), currently documented as version `0.13.0`.

## Criteria from the design

| Criterion | `cel` crate status | Notes |
| --- | --- | --- |
| Nested host values such as `file.ext` and `pdf.text` | Appears supported | Examples show serde-backed structs and field access such as `foo.a == foo.b`. Alder would likely inject nested structs/maps rather than flat dotted identifiers. |
| Custom functions | Supported | Examples show `context.add_function("add", ...)` and method-like functions via `This`. |
| Clear errors | Promising | Examples show syntax errors with source positions. Needs validation with rule-file diagnostics. |
| Deterministic/sandboxed | Promising | CEL is non-Turing-complete and the crate exposes expression execution through a context. Alder should avoid registering functions with I/O. |
| Missing facts | Needs prototype | Alder must decide whether missing fields are hard errors, nulls, or provider-triggering lookups. Current evaluator treats unknown identifiers as errors and explicit null as a soft value. |
| No I/O capability | Supported by policy | The expression engine itself does not need I/O; Alder controls registered functions and context values. |
| Bounded evaluation | Needs prototype | CEL is non-Turing-complete, but Alder should still test very large expressions, comprehensions, regex cost, and nested data. |
| Grammar stability | Better than custom | CEL is a standard language. The Rust crate had breaking changes in recent releases, so Alder should isolate it behind an adapter. |

## Important migration differences

Alder's provisional evaluator treats dotted identifiers as flat fact keys:

```text
file.ext
pdf.text
spotlight.kMDItemAuthors
```

Real CEL treats dots as field selection. Migrating to `cel` should therefore inject nested objects:

```text
file.ext     -> context variable `file` with field `ext`
pdf.text     -> context variable `pdf` with field `text`
spotlight.*  -> context variable `spotlight` with fields such as `kMDItemTextContent`
```

This is compatible with the rule syntax Alder wants, but the fact context construction must change from flat strings to nested values.

## Recommendation

Do not replace the current evaluator immediately.

Instead:

1. Keep `src/expr.rs` as a provisional adapter boundary.
2. Add a second adapter behind the same public API once the pipeline is stable.
3. Prototype `cel` with Alder's exact rule examples:
   - `file.ext == ".pdf"`
   - `contains(pdf.text, "American Express")`
   - `matches(pdf.text, "Closing Date\\\\s+")`
   - missing `pdf.text`
   - nested `spotlight.kMDItemTextContent`
4. Compare diagnostics and JSON explain output before switching defaults.

The near-term decision is: *keep the custom evaluator for now, but treat `cel` as the preferred replacement candidate once an adapter prototype proves missing-fact and diagnostics behavior.*

## Follow-up prototype checklist

- Add an `ExpressionEngine` trait or equivalent adapter seam.
- Implement current evaluator behind that seam.
- Implement experimental `cel` adapter behind a feature flag.
- Convert flat fact maps into nested CEL context values.
- Register safe helper functions or map Alder helpers to CEL idioms:
  - `contains(haystack, needle)`
  - `matches(haystack, regex)`
  - `lower(value)`
- Test all existing expression unit tests against both engines.
- Decide whether missing facts are hard errors or explicit nulls.
