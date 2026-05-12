# Generic date extraction design

Alder's converted Hazel rules currently model custom Hazel date tokens with explicit
`extract.regex + format` entries. That is precise when the source text is known,
but brittle for statement PDFs where the original Hazel token semantics are hidden
in Hazel's archived rule data.

This document proposes a first-class date extractor that reuses common date
scanning logic while keeping Alder's current explicit regex extractor available.
It is a design proposal, not current implemented config syntax.

## Goals

- Represent Hazel-style tokens such as `statement_date`, `bill_date`,
  `stmt_date`, `pay_date`, and `eob_date` without hand-writing a new regex for
  every issuer.
- Support common financial-document formats:
  - `MM/DD/YYYY` and `M/D/YYYY`
  - `YYYY-MM-DD`
  - `Month D, YYYY` and abbreviated month names
  - compact `YYYYMMDD` only when explicitly requested
- Prefer dates near issuer-specific labels such as `Statement Date:`,
  `Close date:`, `Closing Date`, `Bill Date`, or `Pay Date`.
- Reject ambiguous or weak matches instead of silently moving files using the
  wrong date.
- Expose extracted dates to templates in the same canonical `YYYY-MM-DD` string
  shape used by today's `format` field.

## Non-goals

- Reconstruct Hazel's exact archived custom-token implementation.
- Parse every natural-language date phrase.
- Infer statement semantics from arbitrary document text with no rule context.
- Replace explicit regex extractors. Regex remains the escape hatch for unusual
  documents.

## Proposed rule syntax

The minimal useful extractor is label anchored:

```yaml
extract:
  statement_date:
    kind: date
    from: pdf.text
    after: "Statement Date:"
    formats: ["%m/%d/%Y", "%Y-%m-%d", "%B %-d, %Y", "%b %-d, %Y"]
```

A rule with a label variant can keep the variable name stable:

```yaml
extract:
  stmt_date:
    kind: date
    from: pdf.text
    after: "Close date:"
    formats: ["%m/%d/%Y"]
```

For documents where the label can appear on the same line before or after the
value, use `near`:

```yaml
extract:
  bill_date:
    kind: date
    from: pdf.text
    near: "Bill Date"
    window: same_line
    formats: ["%m/%d/%Y", "%B %-d, %Y"]
```

Document-wide scanning should be possible but discouraged because false
positives are common in financial PDFs:

```yaml
extract:
  pay_date:
    kind: date
    from: pdf.text
    scope: document
    formats: ["%m/%d/%Y"]
```

Rules should use `scope: document` only when the rule's `when` predicate already
identifies a document type that contains exactly one relevant date.

## Compatibility with existing extractors

Use a tagged extractor model with `kind` defaulting to `regex` so all existing
configs continue to parse unchanged. Avoid a broad untagged enum for the public
schema because typo diagnostics become confusing when serde tries multiple
shapes.

```rust
pub enum Extractor {
    Regex(RegexExtractor),
    Date(DateExtractor),
}
```

Implement `Deserialize` by hand for the outer enum: inspect the YAML mapping,
then dispatch entries with no `kind` field to `RegexExtractor`, entries with
`kind: date` to `DateExtractor`, and entries with `kind: regex` to
`RegexExtractor`. Each concrete struct should keep `deny_unknown_fields` so typos
still fail clearly. Do not rely on a broad `#[serde(untagged)]` enum for this
compatibility layer; it would make typo diagnostics harder to understand.

Legacy entry without `kind`:

```yaml
extract:
  statement_date:
    from: pdf.text
    regex: "Closing Date\\s+(\\d{2}/\\d{2}/\\d{4})"
    format: "%m/%d/%Y"
```

That legacy syntax should remain valid indefinitely. The date extractor is not
just sugar over one generated regex because it needs reusable candidate scanning,
label normalization, ambiguity handling, and diagnostics. However, it should feed
into the same extraction result map as regex extractors.

## Label matching semantics

`after` and `near` should match labels as normalized literals, not regular
expressions:

- case-insensitive;
- Unicode whitespace collapsed to a single ASCII space;
- optional whitespace around punctuation;
- an optional trailing run of label punctuation such as `:`, `.`, `-`, `#`, and
  surrounding whitespace.

Examples that should match the same label:

- `Statement Date:`
- `Statement date`
- `Statement   Date :`

A future `label_regex` or `pattern` field can provide an escape hatch, but the
MVP should avoid making authors write regexes for the common case.

## Window and selection semantics

The MVP should be deterministic and explainable rather than score-based.
Windows operate on the post-extraction `pdf.text` string. Because `pdftotext`
line breaks are heuristic, especially for tables and multi-column statements,
rules may need `paragraph` or `chars:N` when `same_line` does not match the text
Alder actually sees.

Recommended windows:

| Window | Meaning |
| --- | --- |
| `same_line` | Date appears on the same extracted-text line as the label. Default for `near`. |
| `next_line` | Date appears after the label on the same line or on the following non-empty line. Default for `after`. |
| `paragraph` | Date appears before the next blank line or page break. |
| `chars:N` | Advanced escape hatch for difficult PDFs. |

Selection rules:

1. Locate normalized label occurrences.
2. For each label occurrence, scan only the configured window.
3. Parse date candidates using the configured `formats`.
4. For `after`, return the nearest valid date after the label within the window.
   This handles lines such as `Bill Date: 01/15/2026   Due Date: 02/15/2026`
   without treating the due date as equally relevant.
5. For `near`, return the only valid date in the window. If multiple valid dates
   appear, fail with an ambiguity error and ask the rule author to use `after`,
   a narrower window, or a future explicit selector.
6. For `scope: document`, return a date only when exactly one valid candidate is
   found in the document. Multiple candidates are ambiguous by default.
7. If no date is found near the first label occurrence, try the next label
   occurrence.

Avoid a hidden scoring system in the MVP. If Alder later needs richer selection,
it should be opt-in, for example `select: closest` or `select: last`, and the
explain output should show why the candidate won.

## Date formats

Date extractors should not have a large implicit default format set at first.
Rules should specify accepted formats so date order and ambiguity are visible in
config.

Recommended supported `chrono` formats:

| Example | Format |
| --- | --- |
| `04/15/2026` | `%m/%d/%Y` |
| `4/5/2026` | `%m/%d/%Y` |
| `2026-04-15` | `%Y-%m-%d` |
| `April 15, 2026` | `%B %-d, %Y` |
| `Apr 15, 2026` | `%b %-d, %Y` |
| `20260415` | `%Y%m%d` |

Compact `%Y%m%d` should be opt-in per extractor and should only match when the
candidate is bounded by whitespace, punctuation, or line boundaries. Eight-digit
runs are common in account numbers, invoice numbers, and filenames.

For each candidate, Alder should try all configured formats. If multiple formats
parse the same text to different dates, extraction should fail as ambiguous
rather than silently trusting format order. If multiple formats parse to the same
`NaiveDate`, the candidate is safe. To avoid hidden MDY vs DMY behavior, Alder
should not include `%d/%m/%Y` in any future U.S.-financial preset.

Month-name formats should use Chrono's English month names. Non-English month
names are out of scope for the first implementation unless an explicit locale
option is added later.

## False positive controls

Financial PDFs often contain due dates, payment dates, transaction dates,
statement periods, account numbers, phone numbers, and copyright years. The date
extractor should therefore be conservative by default.

Recommended controls:

- Require `after`, `near`, or another context field unless the rule explicitly
  sets `scope: document`.
- Prefer line and paragraph windows over arbitrary character windows.
- Use a bounded year range as a heuristic, not a universal rule. A reasonable
  default for converted financial statements is `1990..=current_year + 1`, with
  optional `min_year` and `max_year` overrides for older deeds, transcripts,
  medical records, or far-future-dated documents. Tests should pin the clock or
  pass an explicit reference year so `current_year + 1` does not make outcomes
  time-dependent.
- Reject invalid calendar dates through `NaiveDate`, not regex alone.
- Reject candidates embedded in longer digit sequences or words.
- Do not enable compact dates unless `%Y%m%d` is listed in `formats`.
- Fail on ambiguous same-window matches instead of choosing silently.
- Include the source label, matched text, parsed date, and window in explain or
  JSON diagnostics.

## Template exposure

A successful date extractor should insert the variable as a canonical ISO date
string:

```text
statement_date = 2026-04-15
```

This preserves today's template invariant:

```yaml
to: ~/Documents/Amex/{{ statement_date | date('%Y%m%d') }}{{ file.ext }}
```

The `date(format)` filter can continue to expect `YYYY-MM-DD` input. Typed date
values may be introduced later as part of Alder's broader semantic-value work,
but the first date extractor should not require template authors to branch on
extractor kind.

## Error behavior

Date extractors should follow the same planning behavior as regex extractors:

- missing `from` fact: extraction error for the rule;
- no label or no date in the configured context: no-match extraction error;
- invalid date text for all configured formats: date-parse extraction error;
- multiple equally valid dates in an ambiguous context: ambiguity extraction
  error;
- unsafe rendered template value: existing template safety error.

Alder should include enough diagnostics to explain failures without dumping full
PDF text into logs by default.

## Expression helpers

Do not add `contains_date()` or `extract_date()` expression helpers for the MVP.
Extraction already happens after a rule's `when` predicate matches, and templates
already consume extracted variables. Keeping generic date logic in the extractor
stage avoids adding date types and side effects to the provisional CEL-like
expression evaluator.

Potential future helpers can be reconsidered after Alder has typed facts and a
settled expression engine:

```text
contains_date(pdf.text, near: "Statement Date")
extract_date(pdf.text, after: "Statement Date")
```

Until then, `when` should identify the document family, and `extract` should pull
out the date.

## Migration examples

Current brittle converted rule:

```yaml
extract:
  statement_date:
    from: pdf.text
    regex: '(?P<statement_date>\d{1,2}/\d{1,2}/\d{4})'
    format: "%m/%d/%Y"
```

Safer label-anchored version:

```yaml
extract:
  statement_date:
    kind: date
    from: pdf.text
    after: "Statement Date:"
    formats: ["%m/%d/%Y"]
```

Discover close-date rule:

```yaml
extract:
  stmt_date:
    kind: date
    from: pdf.text
    after: "Close date:"
    formats: ["%m/%d/%Y"]
```

Month-name statement:

```yaml
extract:
  statement_date:
    kind: date
    from: pdf.text
    near: "Statement Closing Date"
    formats: ["%B %-d, %Y", "%b %-d, %Y", "%m/%d/%Y"]
```

## Implementation outline

1. Introduce `RegexExtractor` and `DateExtractor` config structs behind a
   backward-compatible `Extractor` enum.
2. Add validation for date extractors:
   - exactly one of `after`, `near`, or `scope: document` for MVP;
   - non-empty `formats`;
   - compact formats require explicit opt-in by appearing in `formats`;
   - year bounds are valid.
3. Implement reusable date candidate scanners in `render` or a new
   `date_extract` module.
4. Normalize source text into line-oriented spans while preserving byte ranges
   for diagnostics.
5. Match labels, choose the search window, parse candidates, and return canonical
   `YYYY-MM-DD`.
6. Add explain/JSON diagnostics for selected date and rejected ambiguous
   candidates.
7. Migrate selected Hazel-converted rules from document-wide regexes to
   label-anchored date extractors once validated against fixture PDFs.

## Testing plan

Unit tests should cover:

- label normalization and optional colon matching;
- `after` on same line and next line;
- `near` with date before and after the label on the same line;
- each supported date format;
- compact `%Y%m%d` opt-in and digit-boundary checks;
- invalid calendar dates such as `02/30/2026`;
- year range rejection;
- ambiguous same-window failures;
- legacy regex extractor compatibility;
- canonical `YYYY-MM-DD` template output.

Fixture tests should use representative, redacted PDF text from local rule
corpora. Keep private corpora and converted personal rule files outside the
published repository, and commit only generic fixtures.

## Open follow-up decisions

- Whether to add named presets such as `preset: us_financial` after enough rules
  share the same `formats` and context conventions.
- Whether `paragraph` windows should treat form-feed page breaks as hard stops.
- Whether explain output should redact surrounding text by default for sensitive
  financial PDFs.
- Whether typed template values should eventually expose dates as `NaiveDate`
  instead of ISO strings.
