use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, fs, io};

#[cfg(test)]
use clap::CommandFactory;
use clap::{Args, Parser, Subcommand};

use alder::config::parse_config_str;
use alder::pipeline::{
    ProcessOptions, destination_roots, explain_file, facts_for_file, process_paths,
};
use alder::watchman::{
    WatchmanGenerateOptions, generate_trigger_commands, parse_watchman_stdin, watchman_check,
    watchman_sync, watchman_unsync,
};

const EXIT_ERROR: u8 = 1;
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

    /// Manage Alder-owned Watchman triggers.
    Watchman(WatchmanArgs),
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
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Produce an action plan without changing the filesystem.
    #[arg(long)]
    dry_run: bool,

    /// Read Watchman trigger JSON from stdin instead of path arguments.
    #[arg(long)]
    from_watchman: bool,
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

#[derive(Debug, Args)]
struct WatchmanArgs {
    #[command(subcommand)]
    command: WatchmanCommand,
}

#[derive(Debug, Subcommand)]
enum WatchmanCommand {
    /// Print generated Watchman trigger commands as JSON.
    Print,

    /// Register or update Alder-owned Watchman triggers.
    Sync,

    /// Verify Watchman triggers match the current config.
    Check,

    /// Remove Alder-owned Watchman triggers.
    Unsync,
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
        Command::Run(args) => run_paths(cli.config.as_ref(), cli.json, args.paths, args.dry_run),
        Command::Ingest(args) => run_ingest(cli.config.as_ref(), cli.json, args),
        Command::Facts(args) => run_facts(cli.config.as_ref(), cli.json, args),
        Command::Explain(args) => run_explain(cli.config.as_ref(), cli.json, args),
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
        Command::Watchman(args) => run_watchman(cli.config.as_ref(), args),
    }
}

fn run_paths(
    config_path: Option<&PathBuf>,
    json: bool,
    paths: Vec<PathBuf>,
    dry_run: bool,
) -> ExitCode {
    let config_path = match resolve_config_path(config_path) {
        Ok(path) => path,
        Err(error) => return error_exit(error),
    };
    let config = match load_config(&config_path) {
        Ok(config) => config,
        Err(error) => return error_exit(error),
    };
    let roots = match destination_roots(&config) {
        Ok(roots) => roots,
        Err(error) => return error_exit(error),
    };
    if !dry_run && roots.is_empty() {
        return error_exit(
            "non-dry-run execution requires defaults.destination_roots in config".to_string(),
        );
    }

    let options = ProcessOptions {
        dry_run,
        destination_roots: roots,
        action_log_path: default_action_log_path(),
        run_id: default_run_id(),
    };
    let results = process_paths(&config, &paths, &options);
    print_results(json, &results)
}

fn run_ingest(config_path: Option<&PathBuf>, json: bool, args: IngestArgs) -> ExitCode {
    if args.from_watchman {
        return parse_watchman_ingest(config_path, json, args.dry_run);
    }

    if args.paths.is_empty() {
        eprintln!("alder ingest: expected PATH... or --from-watchman");
        return ExitCode::from(EXIT_ERROR);
    }

    run_paths(config_path, json, args.paths, args.dry_run)
}

fn parse_watchman_ingest(config_path: Option<&PathBuf>, json: bool, dry_run: bool) -> ExitCode {
    let config_path = match resolve_config_path(config_path) {
        Ok(path) => path,
        Err(error) => return error_exit(error),
    };
    let config = match load_config(&config_path) {
        Ok(config) => config,
        Err(error) => return error_exit(error),
    };
    let Some(watch) = config.watch.as_ref() else {
        return error_exit("config does not define watch settings".to_string());
    };
    let root = match env::var_os("WATCHMAN_ROOT") {
        Some(root) => PathBuf::from(root),
        None => return error_exit("WATCHMAN_ROOT is required with --from-watchman".to_string()),
    };

    let mut input = String::new();
    if let Err(error) = io::Read::read_to_string(&mut io::stdin(), &mut input) {
        return error_exit(format!("failed to read Watchman stdin: {error}"));
    }
    let candidates = match parse_watchman_stdin(&input, root, watch) {
        Ok(candidates) => candidates,
        Err(error) => return error_exit(error.to_string()),
    };
    let roots = match destination_roots(&config) {
        Ok(roots) => roots,
        Err(error) => return error_exit(error),
    };
    if !dry_run && roots.is_empty() {
        return error_exit(
            "non-dry-run execution requires defaults.destination_roots in config".to_string(),
        );
    }
    let options = ProcessOptions {
        dry_run,
        destination_roots: roots,
        action_log_path: default_action_log_path(),
        run_id: default_run_id(),
    };
    let results = process_paths(&config, &candidates, &options);
    print_results(json, &results)
}

fn run_facts(config_path: Option<&PathBuf>, json: bool, args: FileOutputArgs) -> ExitCode {
    let config_path = match resolve_config_path(config_path) {
        Ok(path) => path,
        Err(error) => return error_exit(error),
    };
    let config = match load_config(&config_path) {
        Ok(config) => config,
        Err(error) => return error_exit(error),
    };
    let output = facts_for_file(&config, args.file);
    if json {
        print_json(&output)
    } else {
        for (key, value) in output.facts {
            println!("{key}: {value:?}");
        }
        for error in output.provider_errors {
            eprintln!("provider error: {error}");
        }
        ExitCode::SUCCESS
    }
}

fn run_explain(config_path: Option<&PathBuf>, json: bool, args: FileOutputArgs) -> ExitCode {
    let config_path = match resolve_config_path(config_path) {
        Ok(path) => path,
        Err(error) => return error_exit(error),
    };
    let config = match load_config(&config_path) {
        Ok(config) => config,
        Err(error) => return error_exit(error),
    };
    let result = explain_file(&config, args.file);
    if json {
        print_json(&result)
    } else {
        print_human_result(&result);
        exit_for_results(std::slice::from_ref(&result))
    }
}

fn run_watchman(config_path: Option<&PathBuf>, args: WatchmanArgs) -> ExitCode {
    let config_path = match resolve_config_path(config_path) {
        Ok(path) => path,
        Err(error) => return error_exit(error),
    };
    let config = match load_config(&config_path) {
        Ok(config) => config,
        Err(error) => return error_exit(error),
    };
    let alder_exe = match env::current_exe() {
        Ok(path) => path,
        Err(error) => return error_exit(format!("failed to locate current executable: {error}")),
    };
    let options = WatchmanGenerateOptions::new(config_path, alder_exe);

    match args.command {
        WatchmanCommand::Print => {
            let commands = match generate_trigger_commands(&config, &options) {
                Ok(commands) => commands,
                Err(error) => return error_exit(error.to_string()),
            };
            match serde_json::to_string_pretty(&commands) {
                Ok(output) => {
                    println!("{output}");
                    ExitCode::SUCCESS
                }
                Err(error) => error_exit(format!("failed to encode Watchman commands: {error}")),
            }
        }
        WatchmanCommand::Sync => match watchman_sync(&config, &options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => error_exit(error.to_string()),
        },
        WatchmanCommand::Check => match watchman_check(&config, &options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => error_exit(error.to_string()),
        },
        WatchmanCommand::Unsync => match watchman_unsync(&config, &options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => error_exit(error.to_string()),
        },
    }
}

fn resolve_config_path(config_path: Option<&PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = config_path {
        return std::path::absolute(path)
            .map_err(|error| format!("failed to resolve config path {}: {error}", path.display()));
    }

    for candidate in DEFAULT_CONFIG_CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return std::path::absolute(&path).map_err(|error| {
                format!("failed to resolve config path {}: {error}", path.display())
            });
        }
    }

    Err(format!(
        "no config path provided and none found: {}",
        DEFAULT_CONFIG_CANDIDATES.join(", ")
    ))
}

fn load_config(path: &PathBuf) -> Result<alder::config::Config, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;
    parse_config_str(&input).map_err(|error| error.to_string())
}

fn default_action_log_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/state/alder/actions.jsonl")
}

fn default_run_id() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn print_results(json: bool, results: &[alder::pipeline::PipelineResult]) -> ExitCode {
    if json {
        print_json(results)
    } else {
        for result in results {
            print_human_result(result);
        }
        exit_for_results(results)
    }
}

fn print_json<T: serde::Serialize + ?Sized>(value: &T) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => error_exit(format!("failed to encode JSON: {error}")),
    }
}

fn print_human_result(result: &alder::pipeline::PipelineResult) {
    println!("File: {}", result.source.display());
    for error in &result.provider_errors {
        println!("  Provider error: {error}");
    }
    if let Some(explanation) = &result.explanation {
        for eval in &explanation.rule_evaluations {
            let status = if eval.matched {
                "matched"
            } else {
                "not matched"
            };
            let shadowed = if eval.shadowed { ", shadowed" } else { "" };
            println!("  Rule {}: {status}{shadowed}", eval.rule_id);
            if let Some(error) = &eval.error {
                println!("    Error: {error}");
            }
        }
        if let Some(plan) = &explanation.plan {
            println!("  Plan: {}", plan.rule_id);
            for action in &plan.actions {
                println!("    {action:?}");
            }
        } else {
            println!("  Plan: none");
        }
    }
    if let Some(execution) = &result.execution {
        for record in &execution.records {
            println!(
                "  Executed {}: {:?} -> {}",
                record.action,
                record.status,
                record.destination.display()
            );
        }
    }
    if let Some(error) = &result.error {
        println!("  Error: {error}");
    }
}

fn exit_for_results(results: &[alder::pipeline::PipelineResult]) -> ExitCode {
    if results.iter().any(|result| result.error.is_some()) {
        ExitCode::from(EXIT_ERROR)
    } else {
        ExitCode::SUCCESS
    }
}

fn error_exit(error: String) -> ExitCode {
    eprintln!("alder: {error}");
    ExitCode::from(EXIT_ERROR)
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

    #[test]
    fn parses_watchman_print() {
        let cli =
            Cli::try_parse_from(["alder", "--config", "alder.yaml", "watchman", "print"]).unwrap();

        match cli.command {
            Command::Watchman(args) => assert!(matches!(args.command, WatchmanCommand::Print)),
            other => panic!("expected watchman command, got {other:?}"),
        }
    }

    #[test]
    fn parses_ingest_from_watchman_without_paths() {
        let cli = Cli::try_parse_from(["alder", "ingest", "--from-watchman"]).unwrap();

        match cli.command {
            Command::Ingest(args) => {
                assert!(args.from_watchman);
                assert!(args.paths.is_empty());
            }
            other => panic!("expected ingest command, got {other:?}"),
        }
    }
}
