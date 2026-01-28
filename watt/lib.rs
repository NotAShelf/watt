use std::{
  env,
  path::PathBuf,
};

use anyhow::Context as _;
use clap::Parser as _;

pub mod cpu;
pub mod power_supply;
pub mod system;

pub mod fs;

pub mod config;

pub mod lock;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
pub struct Cli {
  #[command(flatten)]
  verbosity: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

  /// The daemon config path.
  #[arg(long, env = "WATT_CONFIG")]
  config: Option<PathBuf>,

  /// Force running even if another instance is already running. Potentially
  /// destructive.
  #[arg(long)]
  force: bool,
}

pub fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();

  yansi::whenever(yansi::Condition::TTY_AND_COLOR);

  env_logger::Builder::new()
    .filter_level(cli.verbosity.log_level_filter())
    .format_timestamp(None)
    .format_module_path(false)
    .init();

  let config = config::DaemonConfig::load_from(cli.config.as_deref())
    .context("failed to load daemon config")?;

  log::info!("starting watt daemon");

  let lock_path = env::var("XDG_RUNTIME_DIR")
    .map(|dir| PathBuf::from(dir).join("watt.pid"))
    .unwrap_or_else(|_| PathBuf::from("/run/watt.pid"));

  let _lock = lock::LockFile::acquire(&lock_path, cli.force).context(
    format!("failed to acquire pid lock at {}", lock_path.display()),
  )?;

  system::run_daemon(config)
}
