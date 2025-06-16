use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser as _;

pub mod cpu;
pub mod power_supply;
pub mod system;

pub mod fs;

pub mod config;
pub mod daemon;

#[derive(clap::Parser, Debug)]
pub struct Cli {
  #[clap(subcommand)]
  command: Command,
}

#[derive(clap::Parser, Debug)]
#[clap(multicall = true)]
pub enum Command {
  /// Watt daemon.
  Watt {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    #[clap(flatten)]
    command: WattCommand,
  },

  /// CPU metadata and modification utility.
  Cpu {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    #[clap(subcommand)]
    command: CpuCommand,
  },

  /// Power supply metadata and modification utility.
  Power {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    #[clap(subcommand)]
    command: PowerCommand,
  },
}

#[derive(clap::Parser, Debug)]
#[clap(version)]
pub struct WattCommand {
  /// The daemon config path.
  #[arg(long, env = "WATT_CONFIG")]
  config: Option<PathBuf>,
}

#[derive(clap::Parser, Debug)]
#[clap(version)]
pub enum CpuCommand {
  /// Modify CPU attributes.
  Set(config::CpuDelta),
}

#[derive(clap::Parser, Debug)]
#[clap(version)]
pub enum PowerCommand {
  /// Modify power supply attributes.
  Set(config::PowerDelta),
}

pub fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();

  yansi::whenever(yansi::Condition::TTY_AND_COLOR);

  let (Command::Watt { verbosity, .. }
  | Command::Cpu { verbosity, .. }
  | Command::Power { verbosity, .. }) = cli.command;

  env_logger::Builder::new()
    .filter_level(dbg!(verbosity.log_level_filter()))
    .format_timestamp(None)
    .format_module_path(false)
    .init();

  match cli.command {
    Command::Watt {
      command: WattCommand { config },
      ..
    } => {
      let config = config::DaemonConfig::load_from(config.as_deref())
        .context("failed to load daemon config")?;

      daemon::run(config)
    },

    Command::Cpu {
      command: CpuCommand::Set(delta),
      ..
    } => delta.apply(),

    Command::Power {
      command: PowerCommand::Set(delta),
      ..
    } => delta.apply(),
  }
}
