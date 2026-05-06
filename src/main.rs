use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};

const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// Default config discovery order used by future config loading:
///
/// 1. An explicit `--config PATH`, if provided.
/// 2. `./alder.yaml` in the current working directory.
/// 3. `./alder.yml` in the current working directory.
const DEFAULT_CONFIG_CANDIDATES: &[&str] = &["alder.yaml", "alder.yml"];

#[derive(Debug, Parser)]
#[command(version, about = "Plaintext, agent-friendly file routing")]
struct Cli {
    /// Path to the Alder YAML config.
    ///
    /// If omitted, Alder will look for alder.yaml and then alder.yml in the
    /// current working directory.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Emit machine-readable JSON where supported.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan paths, evaluate rules, and optionally execute planned actions.
    Run(RunArgs),

    /// Process explicit candidate files, such as a Watchman event batch.
    Ingest(IngestArgs),

    /// Print facts for one file.
    Facts(FileOutputArgs),

    /// Explain rule evaluation and planned actions for one file.
    Explain(FileOutputArgs),

    /// Run fixture-based rule tests.
    Test,

    /// Undo a previously logged action.
    Undo(UndoArgs),

    /// Print Watchman integration guidance or run a future watcher helper.
    Watch,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Files or directories to scan.
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Produce an action plan without changing the filesystem.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct IngestArgs {
    /// Candidate files to process.
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Produce an action plan without changing the filesystem.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct FileOutputArgs {
    /// File to inspect.
    #[arg(value_name = "FILE")]
    file: PathBuf,
}

#[derive(Debug, Args)]
struct UndoArgs {
    /// Action log entry or selector to undo. Defaults to the most recent action.
    #[arg(value_name = "TARGET")]
    target: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> ExitCode {
    let config_hint = cli
        .config
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| DEFAULT_CONFIG_CANDIDATES.join(" then "));

    match cli.command {
        Command::Run(args) => stub(
            cli.json,
            "run",
            format!(
                "would scan {} path(s), dry_run={}, config={}",
                args.paths.len(),
                args.dry_run,
                config_hint
            ),
        ),
        Command::Ingest(args) => stub(
            cli.json,
            "ingest",
            format!(
                "would process {} candidate file(s), dry_run={}, config={}",
                args.paths.len(),
                args.dry_run,
                config_hint
            ),
        ),
        Command::Facts(args) => stub(
            cli.json,
            "facts",
            format!("would produce facts for {}", args.file.display()),
        ),
        Command::Explain(args) => stub(
            cli.json,
            "explain",
            format!(
                "would explain {} with config={}",
                args.file.display(),
                config_hint
            ),
        ),
        Command::Test => stub(
            cli.json,
            "test",
            format!("would run fixture tests with config={config_hint}"),
        ),
        Command::Undo(args) => stub(
            cli.json,
            "undo",
            format!(
                "would undo {}",
                args.target.as_deref().unwrap_or("last logged action")
            ),
        ),
        Command::Watch => stub(
            cli.json,
            "watch",
            format!("would use Watchman with config={config_hint}"),
        ),
    }
}

fn stub(json: bool, command: &str, detail: String) -> ExitCode {
    if json {
        eprintln!(
            r#"{{"status":"not_implemented","command":"{}","detail":"{}"}}"#,
            escape_json(command),
            escape_json(&detail)
        );
    } else {
        eprintln!("alder {command}: not yet implemented: {detail}");
    }

    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_run_dry_run_with_global_flags() {
        let cli = Cli::try_parse_from([
            "alder",
            "--config",
            "rules.yaml",
            "--json",
            "run",
            "~/Downloads",
            "--dry-run",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("rules.yaml")));
        assert!(cli.json);

        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.paths, vec![PathBuf::from("~/Downloads")]);
                assert!(args.dry_run);
            }
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn parses_optional_undo_target() {
        let cli = Cli::try_parse_from(["alder", "undo"]).unwrap();

        match cli.command {
            Command::Undo(args) => assert_eq!(args.target, None),
            other => panic!("expected undo command, got {other:?}"),
        }
    }
}
