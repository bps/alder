use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

    fn with_pdf(&self, name: &str) -> PathBuf {
        self.write_inbox(name, b"fake pdf")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(alder());
        command.env("HOME", &self.home);
        command.env("XDG_CONFIG_HOME", self.home.join(".config"));
        command.env("XDG_DATA_HOME", self.home.join(".local/share"));
        command.arg("--config").arg(&self.config);
        command
    }

    fn run<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command()
            .args(args)
            .output()
            .expect("failed to execute sandbox command")
    }

    fn action_log(&self) -> PathBuf {
        self.home.join(".local/state/alder/actions.jsonl")
    }
}

#[test]
fn dry_run_does_not_move_files() {
    let sandbox = Sandbox::new();
    let source = sandbox.with_pdf("statement.pdf");

    let output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("run"),
        sandbox.inbox.as_os_str(),
        OsStr::new("--dry-run"),
    ]);

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
    let source = sandbox.with_pdf("statement.pdf");

    let output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("run"),
        sandbox.inbox.as_os_str(),
    ]);

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
fn trash_only_config_does_not_require_destination_roots() {
    let temp = tempfile::tempdir().unwrap();
    let inbox = temp.path().join("inbox");
    let home = temp.path().join("home");
    fs::create_dir_all(&inbox).unwrap();
    fs::create_dir_all(&home).unwrap();
    let config = temp.path().join("alder.yaml");
    fs::write(
        &config,
        r#"
version: 1
rules:
  - id: trash-tmp
    when: file.ext == ".tmp"
    actions:
      - trash: {}
"#,
    )
    .unwrap();

    let output = Command::new(alder())
        .env("HOME", &home)
        .arg("--config")
        .arg(&config)
        .arg("--json")
        .arg("run")
        .arg(&inbox)
        .output()
        .expect("failed to execute sandbox command");

    assert!(output.status.success(), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 0);
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
#[test]
fn trash_restore_by_action_id_round_trips_in_platform_trash() {
    let sandbox = TrashSandbox::new();
    let source = sandbox.write_source("roundtrip.txt", b"restore me");
    #[cfg(target_os = "windows")]
    let _cleanup = WindowsTrashCleanup::new(source.clone());

    let run_output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("run"),
        sandbox.inbox.as_os_str(),
    ]);
    assert!(run_output.status.success(), "{}", stderr(&run_output));
    assert!(!source.exists());
    let trash_record = latest_log_record(&sandbox.action_log(), "trash", "trashed");
    let action_id = trash_record["action_id"].as_str().unwrap().to_string();
    assert!(
        trash_record["trash_time_deleted"].is_i64(),
        "trash record should include platform restore metadata: {trash_record}"
    );

    let undo_output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("undo"),
        OsStr::new(&action_id),
    ]);

    assert!(undo_output.status.success(), "{}", stderr(&undo_output));
    assert_eq!(fs::read(&source).unwrap(), b"restore me");
    let json: Value = serde_json::from_slice(&undo_output.stdout).unwrap();
    assert_eq!(json["status"], "undone");
    assert_eq!(json["undone_action_id"], action_id);
    assert_eq!(
        latest_log_record(&sandbox.action_log(), "undo_trash", "undone")["undoes_action_id"],
        action_id
    );
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
#[test]
#[ignore = "touches the real OS Trash/Recycle Bin; set ALDER_RUN_REAL_OS_TRASH_TESTS=1 to run"]
fn real_os_trash_run_and_undo_by_action_id() {
    if std::env::var_os("ALDER_RUN_REAL_OS_TRASH_TESTS").is_none() {
        eprintln!("skipping real OS trash test; set ALDER_RUN_REAL_OS_TRASH_TESTS=1 to allow it");
        return;
    }

    let sandbox = TrashSandbox::new();
    let source = sandbox.write_source(
        &format!("alder-real-os-trash-test-{}.txt", uuid::Uuid::new_v4()),
        b"real os trash integration test",
    );
    #[cfg(target_os = "windows")]
    let _cleanup = WindowsTrashCleanup::new(source.clone());

    let run_output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("run"),
        sandbox.inbox.as_os_str(),
    ]);

    assert!(run_output.status.success(), "{}", stderr(&run_output));
    assert!(!source.exists(), "source should have moved to OS trash");
    let json: Value = serde_json::from_slice(&run_output.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["execution"]["records"][0]["action"], "trash");
    assert_eq!(json[0]["execution"]["records"][0]["status"], "trashed");

    let trash_record = latest_log_record(&sandbox.action_log(), "trash", "trashed");
    assert_eq!(trash_record["from"], source.to_string_lossy().as_ref());
    assert!(
        trash_record["trash_time_deleted"].is_i64(),
        "expected restore metadata in trash record: {trash_record}"
    );

    let action_id = trash_record["action_id"].as_str().unwrap();
    let undo_output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("undo"),
        OsStr::new(action_id),
    ]);

    assert!(undo_output.status.success(), "{}", stderr(&undo_output));
    assert_eq!(
        fs::read(&source).unwrap(),
        b"real os trash integration test"
    );
    let undo_json: Value = serde_json::from_slice(&undo_output.stdout).unwrap();
    assert_eq!(undo_json["status"], "undone");
    assert_eq!(undo_json["restored_to"], source.to_string_lossy().as_ref());
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[test]
fn trash_restore_refuses_duplicate_same_original_path_matches() {
    let sandbox = TrashSandbox::new();
    let source = sandbox.write_source("duplicate.txt", b"same bytes");

    let first_output = sandbox.run([OsStr::new("run"), sandbox.inbox.as_os_str()]);
    assert!(first_output.status.success(), "{}", stderr(&first_output));
    let first_record = latest_log_record(&sandbox.action_log(), "trash", "trashed");
    let first_action_id = first_record["action_id"].as_str().unwrap().to_string();
    assert!(first_record["trash_time_deleted"].is_i64());
    let first_infos = sandbox.trashinfo_paths_for_source(&source);
    assert_eq!(
        first_infos.len(),
        1,
        "expected the test trash to be isolated under XDG_DATA_HOME/Trash/info"
    );
    let first_deletion_date = deletion_date(&first_infos[0]);

    fs::write(&source, b"same bytes").unwrap();
    let second_output = sandbox.run([OsStr::new("run"), sandbox.inbox.as_os_str()]);
    assert!(second_output.status.success(), "{}", stderr(&second_output));
    let second_infos: Vec<PathBuf> = sandbox
        .trashinfo_paths_for_source(&source)
        .into_iter()
        .filter(|path| !first_infos.contains(path))
        .collect();
    assert_eq!(second_infos.len(), 1);
    // Freedesktop trash records deletion times at one-second precision. Force the
    // two real trash items to share Alder's conservative matching tuple
    // (original path, deletion time, size), so restore must refuse ambiguity.
    set_deletion_date(&second_infos[0], &first_deletion_date);

    let undo_output = sandbox.run([OsStr::new("undo"), OsStr::new(&first_action_id)]);

    assert!(!undo_output.status.success());
    assert!(
        stderr(&undo_output).contains("multiple trash items match action"),
        "{}",
        stderr(&undo_output)
    );
    assert!(!source.exists());
    assert_eq!(sandbox.trashinfo_paths_for_source(&source).len(), 2);
    assert_eq!(
        latest_log_record(&sandbox.action_log(), "undo_trash", "failed")["undoes_action_id"],
        first_action_id
    );
}

#[test]
fn undo_last_restores_last_move() {
    let sandbox = Sandbox::new();
    let source = sandbox.with_pdf("statement.pdf");
    let dest = sandbox.sorted.join("statement.pdf");
    let run_output = sandbox.run([OsStr::new("run"), sandbox.inbox.as_os_str()]);
    assert!(run_output.status.success(), "{}", stderr(&run_output));

    let undo_output = sandbox.run(["--json", "undo"]);

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
    let source = sandbox.with_pdf("statement.pdf");

    let output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("facts"),
        source.as_os_str(),
    ]);

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
    let source = sandbox.with_pdf("statement.pdf");

    let output = sandbox.run([
        OsStr::new("--json"),
        OsStr::new("explain"),
        source.as_os_str(),
    ]);

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

    let output = sandbox.run(["watchman", "print"]);

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
    sandbox.with_pdf("statement.pdf");
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

#[test]
fn stub_json_escapes_control_characters() {
    let output = Command::new(alder())
        .args(["--config", "rules\t.yaml", "--json", "test"])
        .output()
        .expect("failed to execute alder test command");

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let json: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(json["status"], "not_implemented");
    assert_eq!(json["command"], "test");
    assert_eq!(
        json["detail"],
        "would run fixture tests with config=rules\t.yaml"
    );
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
struct TrashSandbox {
    _temp: tempfile::TempDir,
    inbox: PathBuf,
    home: PathBuf,
    xdg_data_home: PathBuf,
    config: PathBuf,
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
impl TrashSandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let inbox = temp.path().join("inbox");
        let home = temp.path().join("home");
        let xdg_data_home = temp.path().join("xdg-data");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&xdg_data_home).unwrap();
        let config = temp.path().join("alder.yaml");
        fs::write(
            &config,
            r#"
version: 1
rules:
  - id: trash-text
    when: file.ext == ".txt"
    actions:
      - trash: {}
"#,
        )
        .unwrap();

        Self {
            _temp: temp,
            inbox,
            home,
            xdg_data_home,
            config,
        }
    }

    fn write_source(&self, name: &str, content: &[u8]) -> PathBuf {
        let source = self.inbox.join(name);
        fs::write(&source, content).unwrap();
        source
    }

    fn command(&self) -> Command {
        let mut command = Command::new(alder());
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .arg("--config")
            .arg(&self.config);
        command
    }

    fn run<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command()
            .args(args)
            .output()
            .expect("failed to execute sandbox command")
    }

    fn action_log(&self) -> PathBuf {
        self.home.join(".local/state/alder/actions.jsonl")
    }

    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    fn trashinfo_paths_for_source(&self, source: &Path) -> Vec<PathBuf> {
        let info_dir = self.xdg_data_home.join("Trash/info");
        let mut paths: Vec<PathBuf> = fs::read_dir(&info_dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", info_dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| trashinfo_original_path(path).as_deref() == Some(source))
            .collect();
        paths.sort();
        paths
    }
}

#[cfg(target_os = "windows")]
struct WindowsTrashCleanup {
    original_path: PathBuf,
}

#[cfg(target_os = "windows")]
impl WindowsTrashCleanup {
    fn new(original_path: PathBuf) -> Self {
        Self { original_path }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsTrashCleanup {
    fn drop(&mut self) {
        let Ok(items) = trash::os_limited::list() else {
            return;
        };
        let leftovers: Vec<_> = items
            .into_iter()
            .filter(|item| item.original_path() == self.original_path)
            .collect();
        let _ = trash::os_limited::purge_all(leftovers);
    }
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn latest_log_record(log: &Path, action: &str, status: &str) -> Value {
    fs::read_to_string(log)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", log.display()))
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rev()
        .find(|record| record["action"] == action && record["status"] == status)
        .unwrap_or_else(|| panic!("missing {action}/{status} record in {}", log.display()))
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
fn deletion_date(trashinfo: &Path) -> String {
    fs::read_to_string(trashinfo)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("DeletionDate=").map(str::to_string))
        .unwrap_or_else(|| panic!("missing DeletionDate in {}", trashinfo.display()))
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
fn trashinfo_original_path(trashinfo: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let text = fs::read_to_string(trashinfo).ok()?;
    let encoded = text.lines().find_map(|line| line.strip_prefix("Path="))?;
    let decoded = percent_decode(encoded.as_bytes())?;
    Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            let high = input.get(index + 1).and_then(|byte| hex_value(*byte))?;
            let low = input.get(index + 2).and_then(|byte| hex_value(*byte))?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(input[index]);
            index += 1;
        }
    }
    Some(decoded)
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
fn set_deletion_date(trashinfo: &Path, deletion_date: &str) {
    let text = fs::read_to_string(trashinfo).unwrap();
    let updated = text
        .lines()
        .map(|line| {
            line.strip_prefix("DeletionDate=")
                .map(|_| format!("DeletionDate={deletion_date}"))
                .unwrap_or_else(|| line.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(trashinfo, format!("{updated}\n")).unwrap();
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[allow(dead_code)]
fn assert_exists(path: &Path) {
    assert!(path.exists(), "{} should exist", path.display());
}
