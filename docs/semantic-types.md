# Semantic type assessment

Alder has some good semantic type boundaries, but the core rule/fact/template path is still fairly stringly typed. This is acceptable for the prototype, but it is now a primary design debt for a safety-sensitive file mover.

## What is already good

### Config model

The config layer is reasonably semantic:

- `Config`
- `WatchConfig`
- `DefaultsConfig`
- `Rule`
- `Extractor`
- `Action`
- `ConflictPolicy`

`ConflictPolicy` and `Action` are good examples because invalid variants become hard to represent after config parsing.

### Execution model

The execution layer has useful semantic types:

- `ExecuteOptions`
- `ExecutionReport`
- `ExecutionRecord`
- `ExecutionStatus`
- action-log records
- `ExecuteError`

This is important because moving files is the highest-risk code path.

### Watchman integration

The Watchman trigger work has a healthy boundary:

- `WatchmanGenerateOptions`
- `TriggerCommand`
- `TriggerDefinition`
- `WatchmanError`

Watchman expressions are built in a small helper layer and stored as `serde_json::Value`, keeping the wire-format construction localized to the Watchman integration.

### Planning

The planning layer is also moving in the right direction:

- `Explanation`
- `RuleEvaluation`
- `ActionPlan`
- `PlannedAction`

These are helpful for explainability and agent-facing output.

## Where Alder is still too stringly typed

### Facts

Facts are currently keyed by strings such as:

```text
file.name
file.ext
pdf.text
spotlight.kMDItemAuthors
```

and passed around as maps like:

```rust
IndexMap<String, Value>
IndexMap<String, String>
```

This means mistakes such as `pdf.txt` or `file.extension` are caught late, if at all.

A stronger shape would introduce semantic fact keys:

```rust
pub enum FactKey {
    File(FileFactKey),
    Pdf(PdfFactKey),
    Spotlight(String),
}

pub enum FileFactKey {
    Path,
    Name,
    Stem,
    Ext,
    Size,
    CreatedAt,
    ModifiedAt,
    Sha256,
}

pub enum PdfFactKey {
    Text,
    Title,
    Author,
    PageCount,
}
```

String parsing and rendering can remain at the edges via `FromStr` and `Display`.

### Fact values

The current expression value model is intentionally small:

```rust
pub enum Value {
    Null,
    Bool(bool),
    String(String),
}
```

The domain wants richer values:

- paths;
- integer sizes;
- timestamps;
- dates;
- arrays from Spotlight;
- hashes;
- durations later.

A likely future shape:

```rust
pub enum FactValue {
    Null,
    Bool(bool),
    String(String),
    Integer(i64),
    Date(NaiveDate),
    DateTime(SystemTime),
    Path(PathBuf),
    StringList(Vec<String>),
}
```

This would let expressions and templates retain type information instead of repeatedly stringifying and reparsing.

### Paths

Many distinct path concepts are currently represented as raw `PathBuf`:

- source path;
- watched root;
- destination root;
- rendered destination path;
- Watchman relative path;
- action log path;
- config path.

These categories have different safety rules. Types worth considering:

```rust
pub struct SourcePath(PathBuf);
pub struct WatchedRoot(PathBuf);
pub struct DestinationRoot(PathBuf);
pub struct DestinationPath(PathBuf);
pub struct WatchmanRelativePath(PathBuf);
pub struct ActionLogPath(PathBuf);
```

The highest-value near-term types are:

- `DestinationRoot`;
- `DestinationPath`;
- `WatchmanRelativePath`.

Those directly protect safety-critical boundaries.

### Templates

Templates currently render from flat string maps. The template context is actually semantic:

- facts;
- extracted variables;
- date values;
- provider facts;
- path-safe values.

A future model could look like:

```rust
pub struct TemplateContext {
    facts: FactMap,
    extracted: ExtractedValues,
}

pub enum TemplateValue {
    String(String),
    Date(NaiveDate),
    PathSegment(String),
}
```

This would let Alder distinguish safe path segments from arbitrary untrusted strings.

### Provider reports

`ProviderReport`, `FactProvider`, and `ProviderStatus` are good starts. But provider report facts are still strings:

```rust
facts: Vec<String>
```

They should eventually become:

```rust
facts: Vec<FactKey>
```

## Risk-ranked gaps

1. Destination roots and destination paths
   - Highest safety value.
   - Prevents accidental root-check tautologies or destination escape mistakes.
2. Fact keys
   - Improves expression validation, provider orchestration, and explain output.
3. Fact values
   - Needed for robust dates, numbers, arrays, and typed comparisons.
4. Template context and template values
   - Needed for safer rendering and better date/path behavior.
5. Watchman relative paths
   - Useful, though current validation is already decent.

## Overall assessment

For a prototype: *B / B+*.

Alder already has semantic types in the major architectural areas: config, actions, planning, execution, and Watchman triggers.

For a safety-sensitive file mover heading toward real use: *C+*.

The core data plane is still too stringly typed:

```text
facts -> expressions -> extractors -> templates
```

That is where many future bugs are likely to appear.

## Recommended incremental refactor

Avoid a large rewrite. Do this incrementally.

First, introduce `FactKey`, `FactValue`, and `FactMap` while preserving string syntax at config/expression boundaries:

```rust
pub struct FactMap {
    values: IndexMap<FactKey, FactValue>,
}
```

Then change internal pipeline/provider code from:

```rust
IndexMap<String, Value>
```

to:

```rust
FactMap
```

Second, introduce explicit destination path types:

```rust
pub struct DestinationRoot(PathBuf);
pub struct DestinationPath(PathBuf);
```

and make the executor accept those instead of raw `PathBuf`.

These two steps would significantly improve safety and maintainability without derailing current momentum.
