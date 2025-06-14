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
#[clap(author, version, about)]
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
    verbosity: clap_verbosity_flag::Verbosity,

    /// The daemon config path.
    #[arg(long, env = "WATT_CONFIG")]
    config: Option<PathBuf>,
  },

  /// CPU metadata and modification utility.
  Cpu {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity,

    #[clap(subcommand)]
    command: CpuCommand,
  },

  /// Power supply metadata and modification utility.
  Power {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity,

    #[clap(subcommand)]
    command: PowerCommand,
  },
}

#[derive(clap::Parser, Debug)]
pub enum CpuCommand {
  /// Modify CPU attributes.
  Set(config::CpuDelta),
}

#[derive(clap::Parser, Debug)]
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
    .filter_level(verbosity.log_level_filter())
    .format_timestamp(None)
    .format_module_path(false)
    .init();

  match cli.command {
    Command::Watt { config, .. } => {
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
