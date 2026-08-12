use alertd::{
    config,
    runtime::{self, RuntimeOptions},
};
use clap::{ArgAction, Parser};
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
}

fn main() {
    let cli = Cli::parse();
    let mut config = exit_on_error(config::load_config(&cli.config));
    if let Some(level) = cli.log_level {
        config.runtime.log_level = level;
        exit_on_error(config::validate_config(&config));
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&config.runtime.log_level))
        .with_target(false)
        .init();
    if cli.check_config {
        println!("OK: {}", cli.config.display());
        return;
    }
    if cli.send_test {
        exit_on_error(runtime::send_test(&config, cli.dry_run));
        return;
    }
    exit_on_error(runtime::run(RuntimeOptions {
        config_path: cli.config,
        dry_run: cli.dry_run,
    }));
}

fn exit_on_error<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|error| {
        eprintln!("alertd: {error}");
        std::process::exit(2);
    })
}
