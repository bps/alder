use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn alder() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_alder"))
}

struct Sandbox {
    _temp: tempfile::TempDir,
    inbox: PathBuf,
    sorted: PathBuf,
    home: PathBuf,
    config: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let inbox = temp.path().join("inbox");
        let sorted = temp.path().join("sorted");
        let home = temp.path().join("home");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&sorted).unwrap();
        fs::create_dir_all(&home).unwrap();
        let config = temp.path().join("alder.yaml");
        fs::write(
            &config,
            format!(
                r#"
version: 1
watch:
  paths:
    - "{}"
  include:
    - "*.pdf"
  ignore:
    - "*.tmp"
defaults:
  conflict: append_counter
  destination_roots:
    - "{}"
rules:
  - id: pdfs
    name: PDFs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "{}/{{{{ file.name }}}}"
"#,
                inbox.display(),
                sorted.display(),
                sorted.display()
            ),
        )
        .unwrap();

        Self {
            _temp: temp,
            inbox,
            sorted,
            home,
            config,
        }
    }

    fn write_inbox(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.inbox.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn command(&self) -> Command {
        let mut command = Command::new(alder());
        command.env("HOME", &self.home);
        command.arg("--config").arg(&self.config);
        command
    }

    fn action_log(&self) -> PathBuf {
        self.home.join(".local/state/alder/actions.jsonl")
    }
}

#[test]
fn dry_run_does_not_move_files() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_inbox("statement.pdf", b"fake pdf");

    let output = sandbox
        .command()
        .arg("--json")
        .arg("run")
        .arg(&sandbox.inbox)
        .arg("--dry-run")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["execution"]["records"][0]["status"], "planned");
    assert!(json[0]["explanation"]["plan"].is_object());
    assert!(source.exists());
    assert!(!sandbox.sorted.join("statement.pdf").exists());
}

#[test]
fn run_moves_and_logs() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_inbox("statement.pdf", b"fake pdf");

    let output = sandbox
        .command()
        .arg("--json")
        .arg("run")
        .arg(&sandbox.inbox)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!source.exists());
    assert_eq!(
        fs::read(sandbox.sorted.join("statement.pdf")).unwrap(),
        b"fake pdf"
    );
    let log = fs::read_to_string(sandbox.action_log()).unwrap();
    assert!(log.contains(r#""status":"in_progress""#));
    assert!(log.contains(r#""status":"moved""#));
}

#[test]
fn undo_last_restores_last_move() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_inbox("statement.pdf", b"fake pdf");
    let dest = sandbox.sorted.join("statement.pdf");
    let run_output = sandbox
        .command()
        .arg("run")
        .arg(&sandbox.inbox)
        .output()
        .unwrap();
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let undo_output = sandbox
        .command()
        .arg("--json")
        .arg("undo")
        .output()
        .unwrap();

    assert!(undo_output.status.success(), "{}", stderr(&undo_output));
    assert!(source.exists());
    assert!(!dest.exists());
    assert_eq!(fs::read(&source).unwrap(), b"fake pdf");
    let json: Value = serde_json::from_slice(&undo_output.stdout).unwrap();
    assert_eq!(json["status"], "undone");
    let log = fs::read_to_string(sandbox.action_log()).unwrap();
    assert!(log.contains(r#""action":"undo_move""#));
}

#[test]
fn facts_json_exposes_provider_reports() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_inbox("statement.pdf", b"fake pdf");

    let output = sandbox
        .command()
        .arg("--json")
        .arg("facts")
        .arg(&source)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["facts"]["file.name"]["value"], "statement.pdf");
    assert!(
        json["provider_reports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|report| { report["provider"] == "file" && report["status"] == "invoked" })
    );
    assert!(
        json["provider_reports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|report| { report["provider"] == "pdf" && report["status"] == "not_required" })
    );
}

#[test]
fn explain_json_includes_matched_rule_and_destination() {
    let sandbox = Sandbox::new();
    let source = sandbox.write_inbox("statement.pdf", b"fake pdf");

    let output = sandbox
        .command()
        .arg("--json")
        .arg("explain")
        .arg(&source)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["explanation"]["rule_evaluations"][0]["rule_id"],
        "pdfs"
    );
    assert_eq!(json["explanation"]["rule_evaluations"][0]["matched"], true);
    assert!(
        json["explanation"]["plan"]["actions"][0]["to"]
            .as_str()
            .unwrap()
            .ends_with("sorted/statement.pdf")
    );
}

#[test]
fn watchman_print_generates_direct_alder_trigger() {
    let sandbox = Sandbox::new();

    let output = sandbox
        .command()
        .arg("watchman")
        .arg("print")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    let trigger = &json[0][2];
    assert_eq!(trigger["append_files"], false);
    assert_eq!(
        trigger["stdin"],
        serde_json::json!(["name", "exists", "type"])
    );
    assert!(
        trigger["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part == "ingest")
    );
    assert!(
        trigger["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part == "--from-watchman")
    );
    assert!(trigger["expression"].to_string().contains("pdf"));
    assert!(trigger["expression"].to_string().contains("tmp"));
}

#[test]
fn ingest_from_watchman_moves_only_matching_candidates() {
    let sandbox = Sandbox::new();
    sandbox.write_inbox("statement.pdf", b"fake pdf");
    sandbox.write_inbox("ignored.pdf.tmp", b"partial");
    sandbox.write_inbox("notes.txt", b"notes");

    let mut child = sandbox
        .command()
        .env("WATCHMAN_ROOT", &sandbox.inbox)
        .arg("--json")
        .arg("ingest")
        .arg("--from-watchman")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            br#"[
  {"name":"statement.pdf","exists":true,"type":"f"},
  {"name":"ignored.pdf.tmp","exists":true,"type":"f"},
  {"name":"deleted.pdf","exists":false,"type":"f"},
  {"name":"folder.pdf","exists":true,"type":"d"}
]"#,
        )
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert!(sandbox.sorted.join("statement.pdf").exists());
    assert!(sandbox.inbox.join("ignored.pdf.tmp").exists());
    assert!(!sandbox.sorted.join("ignored.pdf.tmp").exists());
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[allow(dead_code)]
fn assert_exists(path: &Path) {
    assert!(path.exists(), "{} should exist", path.display());
}
