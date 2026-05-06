use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn alder() -> Command {
    Command::new(env!("CARGO_BIN_EXE_alder"))
}

struct Sandbox {
    _temp: tempfile::TempDir,
    home: PathBuf,
    inbox: PathBuf,
    sorted: PathBuf,
    config: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let inbox = root.join("inbox");
        let sorted = root.join("sorted");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&sorted).unwrap();
        let config = root.join("alder.yaml");
        fs::write(&config, config_text(&inbox, &sorted)).unwrap();

        Self {
            _temp: temp,
            home,
            inbox,
            sorted,
            config,
        }
    }

    fn command(&self) -> Command {
        let mut command = alder();
        command.env("HOME", &self.home);
        command.arg("--config").arg(&self.config);
        command
    }

    fn action_log(&self) -> PathBuf {
        self.home.join(".local/state/alder/actions.jsonl")
    }
}

fn config_text(inbox: &Path, sorted: &Path) -> String {
    format!(
        r#"version: 1

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
    )
}

#[test]
fn run_dry_run_json_plans_without_moving() {
    let sandbox = Sandbox::new();
    let source = sandbox.inbox.join("statement.pdf");
    let dest = sandbox.sorted.join("statement.pdf");
    fs::write(&source, b"fake pdf").unwrap();

    let output = sandbox
        .command()
        .arg("--json")
        .arg("run")
        .arg(&sandbox.inbox)
        .arg("--dry-run")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(source.exists());
    assert!(!dest.exists());
    let json = stdout_json(&output);
    assert_eq!(json[0]["explanation"]["plan"]["rule_id"], "pdfs");
    assert_eq!(json[0]["execution"]["records"][0]["status"], "planned");
}

#[test]
fn run_json_moves_file_and_writes_action_log() {
    let sandbox = Sandbox::new();
    let source = sandbox.inbox.join("statement.pdf");
    let dest = sandbox.sorted.join("statement.pdf");
    fs::write(&source, b"fake pdf").unwrap();

    let output = sandbox
        .command()
        .arg("--json")
        .arg("run")
        .arg(&sandbox.inbox)
        .output()
        .unwrap();

    assert_success(&output);
    assert!(!source.exists());
    assert_eq!(fs::read(&dest).unwrap(), b"fake pdf");
    let json = stdout_json(&output);
    assert_eq!(json[0]["execution"]["records"][0]["status"], "moved");

    let log = fs::read_to_string(sandbox.action_log()).unwrap();
    assert!(log.contains(r#""status":"in_progress""#));
    assert!(log.contains(r#""status":"moved""#));
}

#[test]
fn facts_json_reports_file_facts_and_skipped_expensive_providers() {
    let sandbox = Sandbox::new();
    let source = sandbox.inbox.join("statement.pdf");
    fs::write(&source, b"fake pdf").unwrap();

    let output = sandbox
        .command()
        .arg("--json")
        .arg("facts")
        .arg(&source)
        .output()
        .unwrap();

    assert_success(&output);
    let json = stdout_json(&output);
    assert_eq!(json["facts"]["file.name"]["value"], "statement.pdf");
    assert!(json["provider_reports"].as_array().unwrap().iter().any(|report| {
        report["provider"] == "pdf" && report["status"] == "not_required"
    }));
}

#[test]
fn explain_json_includes_matching_rule_and_destination() {
    let sandbox = Sandbox::new();
    let source = sandbox.inbox.join("statement.pdf");
    let dest = sandbox.sorted.join("statement.pdf");
    fs::write(&source, b"fake pdf").unwrap();

    let output = sandbox
        .command()
        .arg("--json")
        .arg("explain")
        .arg(&source)
        .output()
        .unwrap();

    assert_success(&output);
    let json = stdout_json(&output);
    assert_eq!(json["explanation"]["rule_evaluations"][0]["rule_id"], "pdfs");
    assert_eq!(json["explanation"]["rule_evaluations"][0]["matched"], true);
    assert_eq!(
        json["explanation"]["plan"]["actions"][0]["to"],
        dest.display().to_string()
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

    assert_success(&output);
    let json = stdout_json(&output);
    let trigger = &json[0][2];
    assert_eq!(trigger["append_files"], false);
    assert_eq!(trigger["stdin"], serde_json::json!(["name", "exists", "type"]));
    assert_eq!(trigger["command"][1], "ingest");
    assert_eq!(trigger["command"][2], "--from-watchman");
    assert_eq!(trigger["command"][3], "--config");
}

#[test]
fn ingest_from_watchman_moves_only_matching_candidates() {
    let sandbox = Sandbox::new();
    let pdf = sandbox.inbox.join("statement.pdf");
    let ignored = sandbox.inbox.join("ignored.pdf.tmp");
    fs::write(&pdf, b"fake pdf").unwrap();
    fs::write(&ignored, b"partial").unwrap();

    let input = r#"
[
  {"name":"statement.pdf","exists":true,"type":"f"},
  {"name":"ignored.pdf.tmp","exists":true,"type":"f"},
  {"name":"old.pdf","exists":false,"type":"f"},
  {"name":"folder.pdf","exists":true,"type":"d"}
]
"#;
    let mut command = sandbox.command();
    command
        .env("WATCHMAN_ROOT", &sandbox.inbox)
        .arg("--json")
        .arg("ingest")
        .arg("--from-watchman");
    let output = run_with_stdin(command, input);

    assert_success(&output);
    assert!(!pdf.exists());
    assert!(ignored.exists());
    assert_eq!(fs::read(sandbox.sorted.join("statement.pdf")).unwrap(), b"fake pdf");
    let json = stdout_json(&output);
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["execution"]["records"][0]["status"], "moved");
}

#[test]
fn run_without_destination_roots_fails_for_execute() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let inbox = temp.path().join("inbox");
    let sorted = temp.path().join("sorted");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&sorted).unwrap();
    fs::write(inbox.join("statement.pdf"), b"fake pdf").unwrap();
    let config = temp.path().join("alder.yaml");
    fs::write(
        &config,
        format!(
            r#"version: 1
rules:
  - id: pdfs
    when: file.ext == ".pdf"
    actions:
      - move:
          to: "{}/{{{{ file.name }}}}"
"#,
            sorted.display()
        ),
    )
    .unwrap();

    let output = alder()
        .env("HOME", &home)
        .arg("--config")
        .arg(config)
        .arg("run")
        .arg(inbox)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr_string(&output).contains("destination_roots"));
}

fn run_with_stdin(mut command: Command, input: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout_string(output),
        stderr_string(output)
    );
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "failed to parse stdout as JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            stdout_string(output),
            stderr_string(output)
        )
    })
}

fn stdout_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
