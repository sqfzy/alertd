use alertd::{
    config,
    maintenance::{self, MaintenanceStatus},
    runtime::{self, RuntimeOptions},
    state,
};
use chrono::{Local, Utc};
use clap::{ArgAction, Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "Lightweight configuration-driven alert daemon")]
struct Cli {
    #[arg(long, default_value = "/etc/alertd/alertd.toml")]
    config: PathBuf,
    #[arg(long, action = ArgAction::SetTrue)]
    check_config: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    dry_run: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    send_test: bool,
    #[arg(long)]
    log_level: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Control a persistent maintenance window.
    Maintenance {
        #[command(subcommand)]
        action: MaintenanceAction,
    },
}

#[derive(Debug, Subcommand)]
enum MaintenanceAction {
    /// Pause check alerts immediately until an absolute time.
    Start {
        #[arg(long)]
        until: String,
        #[arg(long)]
        reason: String,
    },
    /// Show the current maintenance state.
    Status,
    /// End the current maintenance window.
    Cancel,
}

fn main() {
    let cli = Cli::parse();
    exit_on_error(validate_cli_mode(&cli));
    let mut loaded_config = exit_on_error(config::load_config_with_sha256(&cli.config));
    if let Some(level) = cli.log_level {
        loaded_config.config.runtime.log_level = level;
        exit_on_error(config::validate_config(&loaded_config.config));
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&loaded_config.config.runtime.log_level))
        .with_target(false)
        .init();
    if let Some(command) = cli.command {
        exit_on_error(run_command(
            command,
            &loaded_config.config.runtime.state_dir,
        ));
        return;
    }
    if cli.check_config {
        println!("OK: {}", cli.config.display());
        return;
    }
    if cli.send_test {
        exit_on_error(runtime::send_test(
            &loaded_config.config,
            loaded_config.source_sha256,
            cli.dry_run,
        ));
        return;
    }
    exit_on_error(runtime::run(RuntimeOptions {
        config_path: cli.config,
        loaded_config,
        dry_run: cli.dry_run,
    }));
}

fn validate_cli_mode(cli: &Cli) -> Result<(), &'static str> {
    if cli.command.is_some() && (cli.check_config || cli.dry_run || cli.send_test) {
        return Err(
            "maintenance commands cannot be combined with --check-config, --dry-run, or --send-test",
        );
    }
    Ok(())
}

fn run_command(command: Command, state_dir: &std::path::Path) -> Result<(), String> {
    match command {
        Command::Maintenance { action } => run_maintenance(action, state_dir),
    }
}

fn run_maintenance(action: MaintenanceAction, state_dir: &std::path::Path) -> Result<(), String> {
    match action {
        MaintenanceAction::Start { until, reason } => {
            let now = Utc::now();
            let until = maintenance::parse_until(&until, now).map_err(|e| e.to_string())?;
            let window =
                maintenance::start(state_dir, until, reason, now).map_err(|e| e.to_string())?;
            println!(
                "maintenance requested: id={} until={} reason={}",
                window.id,
                local_time(window.until),
                window.reason
            );
        }
        MaintenanceAction::Status => print_maintenance_status(state_dir)?,
        MaintenanceAction::Cancel => {
            match maintenance::cancel(state_dir).map_err(|e| e.to_string())? {
                Some(window) => println!(
                    "maintenance cancellation requested: id={} reason={}",
                    window.id, window.reason
                ),
                None => println!("no maintenance window"),
            }
        }
    }
    Ok(())
}

fn print_maintenance_status(state_dir: &std::path::Path) -> Result<(), String> {
    let window = maintenance::load(state_dir).map_err(|e| e.to_string())?;
    let persistent = state::load(state_dir).map_err(|e| e.to_string())?;
    let status = maintenance::status(
        window.as_ref(),
        persistent.maintenance_start_notice_id.as_deref(),
        persistent.maintenance_end_notice_id.as_deref(),
        Utc::now(),
    );
    let label = match status {
        MaintenanceStatus::PendingStartNotice => "waiting for daemon confirmation",
        MaintenanceStatus::Active => "active",
        MaintenanceStatus::PendingEndNotice => "cancelled or expired; waiting for end notice",
        MaintenanceStatus::None => "none",
    };
    println!("maintenance: {label}");
    if let Some(window) = window {
        println!("id: {}", window.id);
        println!("until: {}", local_time(window.until));
        println!("reason: {}", window.reason);
    }
    Ok(())
}

fn local_time(value: chrono::DateTime<chrono::FixedOffset>) -> String {
    value
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

fn exit_on_error<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|error| {
        eprintln!("alertd: {error}");
        std::process::exit(2);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_maintenance_commands() {
        let cli = Cli::try_parse_from([
            "alertd",
            "--config",
            "/tmp/alertd.toml",
            "maintenance",
            "start",
            "--until",
            "2036-08-21T16:00:00+08:00",
            "--reason",
            "deploy",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Maintenance {
                action: MaintenanceAction::Start { .. }
            })
        ));
    }

    #[test]
    fn maintenance_requires_start_arguments() {
        assert!(Cli::try_parse_from(["alertd", "maintenance", "start"]).is_err());
    }

    #[test]
    fn maintenance_conflicts_with_daemon_modes() {
        for mode in ["--check-config", "--dry-run", "--send-test"] {
            let cli = Cli::try_parse_from(["alertd", mode, "maintenance", "status"]).unwrap();
            assert!(validate_cli_mode(&cli).is_err());
        }
    }
}
