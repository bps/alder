use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs, io};

#[cfg(test)]
use clap::CommandFactory;
use clap::{Args, Parser, Subcommand};

use alder::config::{Action, Config, parse_config_str};
use alder::execute::{undo_last_move, undo_trash_by_action_id};
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
/// 4. `$XDG_CONFIG_HOME/alder/alder.yaml`, then `.yml`.
/// 5. `$HOME/.config/alder/alder.yaml`, then `.yml`, when XDG_CONFIG_HOME is unset,
///    empty, or relative.
const LOCAL_CONFIG_CANDIDATES: &[&str] = &["alder.yaml", "alder.yml"];

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
    match run(cli) {
        Ok(code) => code,
        Err(error) => error_exit(error),
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let config_hint = cli
        .config
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(default_config_hint);

    match cli.command {
        Command::Run(args) => run_paths(cli.config.as_deref(), cli.json, args.paths, args.dry_run),
        Command::Ingest(args) => run_ingest(cli.config.as_deref(), cli.json, args),
        Command::Facts(args) => run_facts(cli.config.as_deref(), cli.json, args),
        Command::Explain(args) => run_explain(cli.config.as_deref(), cli.json, args),
        Command::Test => Ok(stub(
            cli.json,
            "test",
            format!("would run fixture tests with config={config_hint}"),
        )),
        Command::Undo(args) => run_undo(cli.json, args),
        Command::Watch => Ok(stub(
            cli.json,
            "watch",
            format!("would use Watchman with config={config_hint}"),
        )),
        Command::Watchman(args) => run_watchman(cli.config.as_deref(), args),
    }
}

fn run_paths(
    config_path: Option<&Path>,
    json: bool,
    paths: Vec<PathBuf>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let config_path = resolve_config_path(config_path)?;
    let config = load_config(&config_path)?;
    process_candidates(&config, json, &paths, dry_run)
}

fn run_ingest(
    config_path: Option<&Path>,
    json: bool,
    args: IngestArgs,
) -> Result<ExitCode, String> {
    if args.from_watchman {
        return parse_watchman_ingest(config_path, json, args.dry_run);
    }

    if args.paths.is_empty() {
        return Err("ingest: expected PATH... or --from-watchman".to_string());
    }

    run_paths(config_path, json, args.paths, args.dry_run)
}

fn parse_watchman_ingest(
    config_path: Option<&Path>,
    json: bool,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let config_path = resolve_config_path(config_path)?;
    let config = load_config(&config_path)?;
    let Some(watch) = config.watch.as_ref() else {
        return Err("config does not define watch settings".to_string());
    };
    let root = env::var_os("WATCHMAN_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| "WATCHMAN_ROOT is required with --from-watchman".to_string())?;

    let mut input = String::new();
    io::Read::read_to_string(&mut io::stdin(), &mut input)
        .map_err(|error| format!("failed to read Watchman stdin: {error}"))?;
    let candidates =
        parse_watchman_stdin(&input, root, watch).map_err(|error| error.to_string())?;
    process_candidates(&config, json, &candidates, dry_run)
}

fn process_candidates(
    config: &Config,
    json: bool,
    paths: &[PathBuf],
    dry_run: bool,
) -> Result<ExitCode, String> {
    let roots = destination_roots(config)?;
    if !dry_run && roots.is_empty() && config_needs_destination_roots(config) {
        return Err(
            "non-dry-run execution requires defaults.destination_roots in config".to_string(),
        );
    }

    let options = ProcessOptions {
        dry_run,
        notifications: !dry_run,
        destination_roots: roots,
        action_log_path: default_action_log_path(),
        run_id: default_run_id(),
    };
    let results = process_paths(config, paths, &options);
    print_results(json, &results)
}

fn run_facts(
    config_path: Option<&Path>,
    json: bool,
    args: FileOutputArgs,
) -> Result<ExitCode, String> {
    let config_path = resolve_config_path(config_path)?;
    let config = load_config(&config_path)?;
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
        Ok(ExitCode::SUCCESS)
    }
}

fn run_explain(
    config_path: Option<&Path>,
    json: bool,
    args: FileOutputArgs,
) -> Result<ExitCode, String> {
    let config_path = resolve_config_path(config_path)?;
    let config = load_config(&config_path)?;
    let result = explain_file(&config, args.file);
    if json {
        print_json(&result)
    } else {
        print_human_result(&result);
        Ok(exit_for_results(std::slice::from_ref(&result)))
    }
}

fn run_undo(json: bool, args: UndoArgs) -> Result<ExitCode, String> {
    let report = match args.target.as_deref() {
        None | Some("last") => {
            undo_last_move(&default_action_log_path()).map_err(|error| error.to_string())?
        }
        Some(action_id) => {
            uuid::Uuid::parse_str(action_id).map_err(|_| {
                "undo target must be `last` or an action_id UUID for a trash action".to_string()
            })?;
            undo_trash_by_action_id(&default_action_log_path(), action_id)
                .map_err(|error| error.to_string())?
        }
    };
    if json {
        print_json(&report)
    } else {
        if let Some(restored_from) = &report.restored_from {
            println!(
                "Undid move: {} -> {}",
                restored_from.display(),
                report.restored_to.display()
            );
        } else {
            println!("Restored trash action to {}", report.restored_to.display());
        }
        Ok(ExitCode::SUCCESS)
    }
}

fn run_watchman(config_path: Option<&Path>, args: WatchmanArgs) -> Result<ExitCode, String> {
    let config_path = resolve_config_path(config_path)?;
    let config = load_config(&config_path)?;
    let alder_exe = env::current_exe()
        .map_err(|error| format!("failed to locate current executable: {error}"))?;
    let options = WatchmanGenerateOptions::new(config_path, alder_exe);

    match args.command {
        WatchmanCommand::Print => {
            let commands =
                generate_trigger_commands(&config, &options).map_err(|error| error.to_string())?;
            let output = serde_json::to_string_pretty(&commands)
                .map_err(|error| format!("failed to encode Watchman commands: {error}"))?;
            println!("{output}");
            Ok(ExitCode::SUCCESS)
        }
        WatchmanCommand::Sync => {
            watchman_sync(&config, &options).map_err(|error| error.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        WatchmanCommand::Check => {
            watchman_check(&config, &options).map_err(|error| error.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        WatchmanCommand::Unsync => {
            watchman_unsync(&config, &options).map_err(|error| error.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn resolve_config_path(config_path: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = config_path {
        return std::path::absolute(path)
            .map_err(|error| format!("failed to resolve config path {}: {error}", path.display()));
    }

    let candidates = default_config_candidates();
    for path in &candidates {
        if path.exists() {
            return std::path::absolute(path).map_err(|error| {
                format!("failed to resolve config path {}: {error}", path.display())
            });
        }
    }

    Err(format!(
        "no config path provided and none found: {}",
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn default_config_candidates() -> Vec<PathBuf> {
    LOCAL_CONFIG_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .chain(xdg_config_candidates(
            env::var_os("HOME"),
            env::var_os("XDG_CONFIG_HOME"),
        ))
        .collect()
}

fn default_config_hint() -> String {
    default_config_candidates()
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" then ")
}

fn xdg_config_candidates(
    home: Option<OsString>,
    xdg_config_home: Option<OsString>,
) -> Vec<PathBuf> {
    let config_home = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(PathBuf::from).map(|home| home.join(".config")));

    let Some(config_home) = config_home else {
        return Vec::new();
    };

    vec![
        config_home.join("alder/alder.yaml"),
        config_home.join("alder/alder.yml"),
    ]
}

fn load_config(path: &Path) -> Result<alder::config::Config, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;
    parse_config_str(&input).map_err(|error| error.to_string())
}

fn config_needs_destination_roots(config: &Config) -> bool {
    // Keep this in sync with planning's MVP rule: only the first action is planned.
    config
        .rules
        .iter()
        .any(|rule| matches!(rule.actions.first(), Some(Action::Move(_))))
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

fn print_results(
    json: bool,
    results: &[alder::pipeline::PipelineResult],
) -> Result<ExitCode, String> {
    if json {
        print_json(results)
    } else {
        for result in results {
            print_human_result(result);
        }
        Ok(exit_for_results(results))
    }
}

fn print_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<ExitCode, String> {
    let output = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to encode JSON: {error}"))?;
    println!("{output}");
    Ok(ExitCode::SUCCESS)
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
            for diagnostic in &plan.extraction_diagnostics {
                if let Some(selected) = &diagnostic.selected {
                    let date = selected.date.as_deref().unwrap_or("unparsed");
                    println!(
                        "    Extracted {} from {}: {:?} -> {}",
                        diagnostic.variable, diagnostic.fact, selected.text, date
                    );
                }
                for conflict in &diagnostic.conflicts {
                    println!(
                        "    Conflicting {} near {:?}: {:?} -> {}",
                        diagnostic.variable, conflict.matched_label, conflict.text, conflict.date
                    );
                }
            }
            for action in &plan.actions {
                println!("    {action:?}");
            }
        } else {
            println!("  Plan: none");
        }
    }
    if let Some(execution) = &result.execution {
        for record in &execution.records {
            if let Some(destination) = &record.destination {
                println!(
                    "  Executed {}: {:?} -> {}",
                    record.action,
                    record.status,
                    destination.display()
                );
            } else if let Some(reason) = &record.reason {
                println!(
                    "  Executed {}: {:?} ({reason})",
                    record.action, record.status
                );
                for supporting_file in &record.supporting_files {
                    println!("    Candidate: {}", supporting_file.display());
                }
            } else {
                println!("  Executed {}: {:?}", record.action, record.status);
            }
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
            "{}",
            serde_json::json!({
                "status": "not_implemented",
                "command": command,
                "detail": detail,
            })
        );
    } else {
        eprintln!("alder {command}: not yet implemented: {detail}");
    }

    ExitCode::from(EXIT_NOT_IMPLEMENTED)
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
    fn xdg_candidates_prefer_absolute_xdg_config_home() {
        let candidates = xdg_config_candidates(
            Some(OsString::from("/home/alice")),
            Some(OsString::from("/tmp/xdg")),
        );

        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/tmp/xdg/alder/alder.yaml"),
                PathBuf::from("/tmp/xdg/alder/alder.yml"),
            ]
        );
    }

    #[test]
    fn xdg_candidates_fall_back_to_home_for_empty_or_relative_xdg_config_home() {
        for xdg in [OsString::new(), OsString::from("relative")] {
            let candidates = xdg_config_candidates(Some(OsString::from("/home/alice")), Some(xdg));

            assert_eq!(
                candidates,
                vec![
                    PathBuf::from("/home/alice/.config/alder/alder.yaml"),
                    PathBuf::from("/home/alice/.config/alder/alder.yml"),
                ]
            );
        }
    }

    #[test]
    fn xdg_candidates_are_empty_without_home_or_absolute_xdg_config_home() {
        assert!(xdg_config_candidates(None, None).is_empty());
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
