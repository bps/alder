# Config reference

Alder config is YAML. Unknown keys are rejected by default.

## Top-level keys

```yaml
version: 1
watch: {}
stabilize: {}
defaults: {}
rules: []
```

### `version`

Required integer. The only supported value is `1`.

### `watch`

Optional watch configuration used by `alder watchman *` and `ingest --from-watchman`.

```yaml
watch:
  paths:
    - ~/Downloads
  include:
    - "*.pdf"
  ignore:
    - "*.tmp"
    - "*.download"
  settle: 5s
```

Fields:

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `paths` | list of strings | `[]` | Directories Alder asks Watchman to watch. Required for `watchman sync`. |
| `include` | list of globs | `[]` | Candidate include patterns. Empty means include everything Watchman reports. |
| `ignore` | list of globs | `[]` | Candidate ignore patterns. Alder re-applies these internally; Watchman filtering is only a prefilter. |
| `settle` | string | unset | Parsed as config data today, but not yet applied by Alder-managed Watchman sync. |

### `stabilize`

Optional stabilization configuration.

```yaml
stabilize:
  unchanged_for: 3s
  timeout: 60s
```

Fields are parsed but not fully enforced by the current CLI pipeline yet.

| Field | Type | Default |
| --- | --- | --- |
| `unchanged_for` | string | unset |
| `timeout` | string | unset |

### `defaults`

Optional defaults shared by rules/actions.

```yaml
defaults:
  conflict: append_counter
  destination_roots:
    - ~/Documents
  unmatched:
    move_to: ~/Documents/_Review/{{ file.name }}
```

Fields:

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `conflict` | conflict policy | `append_counter` in planning | Used by move actions unless action-level `conflict` is set. |
| `destination_roots` | list of strings | `[]` | Required for non-dry-run `move` execution. Move destinations must remain inside these roots. Not required for `trash`. |
| `unmatched.move_to` | string | unset | Parsed for design compatibility; not yet executed as a fallback action. |

Conflict policies:

- `error`
- `skip`
- `append_counter`
- `replace_if_same_hash`
- `review`

### `rules`

List of rules evaluated in order.

```yaml
rules:
  - id: pdfs
    name: PDFs
    when: file.ext == ".pdf"
    extract: {}
    actions:
      - move:
          to: ~/Documents/PDFs/{{ file.name }}
```

Fields:

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `id` | string | required | Stable rule identifier for logs/tests/explain. Must be unique and non-empty. |
| `name` | string | unset | Human-readable label. |
| `when` | expression string | required | Provisional CEL-like expression. Must evaluate to bool. |
| `extract` | map | `{}` | Regex extractors keyed by variable name. |
| `actions` | list | required non-empty | Currently `move` and `trash` are executed. Other action shapes parse but planning reports unsupported. |

## Expressions

The current evaluator is provisional and intentionally small.

Supported:

- string literals: `".pdf"`
- booleans: `true`, `false`
- dotted fact identifiers: `file.ext`, `pdf.text`
- `==`, `!=`
- `&&`, `||`
- parentheses
- functions:
  - `contains(haystack, needle)`
  - `matches(haystack, regex)`
  - `lower(value)`

Example:

```yaml
when: |
  file.ext == ".pdf" &&
  contains(pdf.text, "American Express")
```

## Extractors

Extractors create variables for templates. Existing extractors are regex
extractors; `kind: regex` is optional for backward compatibility.

```yaml
extract:
  statement_date:
    from: pdf.text
    regex: "Closing Date\\s+(\\d{2}/\\d{2}/\\d{4})"
    format: "%m/%d/%Y"
```

Fields:

| Field | Type | Notes |
| --- | --- | --- |
| `from` | fact key | Source fact, e.g. `pdf.text`. |
| `regex` | regex string | First match wins. Named capture matching the variable name is preferred; otherwise capture group 1; otherwise the full match. |
| `format` | date parse format | Optional chrono-style date parse format. If present, the extracted date is canonicalized to `YYYY-MM-DD`. |

Date extractors scan for date candidates near normalized literal labels and
return canonical `YYYY-MM-DD` strings:

```yaml
extract:
  statement_date:
    kind: date
    from: pdf.text
    after: "Statement Date:"
    formats: ["%m/%d/%Y", "%Y-%m-%d", "%B %-d, %Y", "%b %-d, %Y"]
```

Fields:

| Field | Type | Notes |
| --- | --- | --- |
| `kind` | `date` | Required for date extractors. |
| `from` | fact key | Source fact, e.g. `pdf.text`. |
| `after` | string | Match a normalized literal label and choose the nearest valid date after it. Exactly one of `after`, `near`, or `scope: document` is required. |
| `near` | string | Match a normalized literal label and require exactly one distinct valid date in the window. |
| `scope` | `document` | Scan the whole fact and require exactly one distinct valid date. Conservative escape hatch for already-specific rules. |
| `window` | `same_line`, `next_line`, `paragraph`, or `chars:N` | Defaults to `next_line` for `after` and `same_line` for `near`. |
| `formats` | list of chrono date formats | Required and non-empty. Compact `%Y%m%d` candidates are scanned only when this format is listed. |
| `min_year` / `max_year` | integer | Optional year bounds. Defaults are `1990` and current year + 1. |

Label matching is case-insensitive, collapses whitespace, allows optional
whitespace around label punctuation, and accepts optional trailing punctuation
such as `:`, `.`, `-`, or `#`. Ambiguous windows fail instead of choosing a date
silently. The first label occurrence with a valid date still wins, but later
matching label occurrences that would select a different valid date are reported
as diagnostic conflicts. JSON explain output includes date extraction diagnostics
for selected labels, candidates, and conflicts.

## Templates

Alder uses Minijinja templates with strict unknown-variable behavior.

Examples:

```yaml
to: ~/Documents/PDFs/{{ file.name }}
to: ~/Documents/Amex/{{ statement_date | date('%Y') }}/{{ statement_date }} - Amex.pdf
```

Supported Alder-specific filter:

- `date(format)` formats an ISO `YYYY-MM-DD` date using a chrono-style format string.

Template safety:

- unknown variables error;
- untrusted variable values may not contain path separators, NULs, or control characters;
- rendered paths may not contain `..`;
- non-dry-run move destinations must remain under `defaults.destination_roots`.

## Move action

```yaml
actions:
  - move:
      to: ~/Documents/PDFs/{{ file.name }}
      conflict: append_counter
```

Fields:

| Field | Type | Default |
| --- | --- | --- |
| `to` | template string | required |
| `conflict` | conflict policy | `defaults.conflict`, then `append_counter` |

## Trash action

```yaml
actions:
  - trash: {}
```

`trash` has no fields today. `trash: {}` and `trash:` both parse as the same empty action.

Trash actions move the source file to the operating system's Trash or Recycle Bin instead of deleting it or moving it to an Alder-managed folder. Alder uses platform trash mechanics:

- macOS uses Finder-compatible trash behavior where possible, so files appear in the user's Trash and can normally be restored with Finder's Put Back behavior.
- Linux follows FreeDesktop.org Trash behavior through the platform trash implementation.
- Windows uses Recycle Bin semantics through the platform trash implementation.

Safety behavior:

- dry-runs produce a planned `trash` execution record and do not mutate the filesystem or action log;
- non-dry-run trash validates that the source is a regular non-symlink file;
- trash actions append explicit action-log records with source sizes for audit;
- `defaults.destination_roots` is not required because there is no Alder destination path to validate;
- `alder undo` does not automatically restore trash actions. Restore from the operating system Trash/Recycle Bin. If the latest action is `trash`, `alder undo` refuses rather than reaching past it to undo an older move.

`move` and `trash` are executed by the current CLI pipeline.

Other parsed action shapes:

- `copy`
- `rename`
- `tag`
- `review`
- `move_to_review`

These are schema placeholders and currently report unsupported during planning.
