# JSON output

Alder's operational commands support global `--json`.

The current JSON shapes are useful for tests and local automation, but they are not yet wrapped in a formal versioned envelope. Treat this document as the current MVP contract, not a long-term compatibility guarantee.

## `run --json` and `ingest --json`

`run` and `ingest` return an array of per-file results.

```json
[
  {
    "source": "/path/inbox/statement.pdf",
    "provider_errors": [],
    "provider_reports": [
      {
        "provider": "file",
        "status": "invoked",
        "facts": ["file.path", "file.name", "file.stem", "file.ext", "file.size"],
        "message": null
      }
    ],
    "explanation": {
      "source": "/path/inbox/statement.pdf",
      "facts": {
        "file.name": { "type": "string", "value": "statement.pdf" }
      },
      "rule_evaluations": [
        {
          "rule_id": "pdfs",
          "rule_name": "PDFs",
          "matched": true,
          "shadowed": false,
          "error": null
        }
      ],
      "plan": {
        "source": "/path/inbox/statement.pdf",
        "rule_id": "pdfs",
        "rule_name": "PDFs",
        "variables": {},
        "actions": [
          {
            "action": "move",
            "to": "/path/sorted/statement.pdf",
            "conflict": "append_counter",
            "terminal": true
          }
        ]
      }
    },
    "execution": {
      "records": [
        {
          "action": "move",
          "source": "/path/inbox/statement.pdf",
          "destination": "/path/sorted/statement.pdf",
          "status": "planned",
          "reason": null,
          "rule_id": "pdfs",
          "sha256": null,
          "size": null
        }
      ]
    },
    "error": null
  }
]
```

Execution statuses currently include:

- `planned`
- `in_progress`
- `moved`
- `skipped`
- `failed`
- `deduped`
- `undone`

For dry-runs, execution records use `planned` and no filesystem mutation occurs.

## `facts --json`

`facts` returns a single facts object.

```json
{
  "source": "/path/file.pdf",
  "facts": {
    "file.name": { "type": "string", "value": "file.pdf" },
    "file.ext": { "type": "string", "value": ".pdf" }
  },
  "provider_errors": [],
  "provider_reports": [
    {
      "provider": "pdf",
      "status": "not_required",
      "facts": [],
      "message": null
    }
  ]
}
```

Provider statuses:

- `not_required`
- `skipped`
- `invoked`
- `error`

## `explain --json`

`explain` returns one per-file result with no execution report unless a future explain mode adds execution simulation details.

Important fields:

- `provider_reports`
- `explanation.facts`
- `explanation.rule_evaluations`
- `explanation.plan`
- `error`

## `undo --json`

`undo` returns an undo report.

```json
{
  "undone_action_id": "550e8400-e29b-41d4-a716-446655440000",
  "restored_from": "/path/sorted/statement.pdf",
  "restored_to": "/path/inbox/statement.pdf",
  "status": "undone"
}
```

## `watchman print`

`watchman print` returns Watchman JSON commands. Each item is a command array suitable for Watchman's JSON protocol.

```json
[
  [
    "trigger",
    "/path/inbox",
    {
      "name": "alder",
      "expression": ["allof", ["type", "f"], ["anyof", ["suffix", "pdf"]]],
      "command": ["/path/to/alder", "ingest", "--from-watchman", "--config", "/path/alder.yaml"],
      "append_files": false,
      "stdin": ["name", "exists", "type"]
    }
  ]
]
```

The key invariants are:

- Alder is invoked directly, not through a shell wrapper;
- `append_files` is `false`;
- Watchman passes structured stdin fields `name`, `exists`, and `type`.

## Action log JSONL

The action log is append-only JSONL, not the same as CLI JSON output.

Move execution writes at least:

1. `action = "move", status = "in_progress"`
2. `action = "move", status = "moved"`

Undo writes:

1. `action = "undo_move", status = "in_progress"`
2. `action = "undo_move", status = "undone"`

Hash dedupe writes:

- `action = "move", status = "deduped"`

Each action log record includes a per-action `action_id` for pairing and reconciliation.
