use std::{
  io::{
    self,
    Write,
  },
  path::PathBuf,
};

use anyhow::{
  Context as _,
  bail,
};
use clap::Parser as _;
use tokio::runtime::Builder as RuntimeBuilder;

pub mod audio;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod power_supply;
pub mod system;
pub mod uncore;
pub mod usb;
pub mod vm;

pub mod fs;

pub mod config;

pub mod lock;

pub mod dbus;
#[cfg(feature = "metrics")] pub mod metrics;
pub mod profile;

#[cfg(test)] mod test_support;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
pub struct Cli {
  #[command(flatten)]
  verbosity: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

  /// The daemon config path.
  #[arg(long, env = "WATT_CONFIG")]
  config: Option<PathBuf>,

  /// Check configuration and exit. Hardware mode also checks discovered CPU,
  /// power-supply and peripheral controls.
  #[arg(
    long,
    value_enum,
    num_args = 0..=1,
    default_missing_value = "offline",
    require_equals = true
  )]
  verify_config: Option<VerificationMode>,

  #[command(subcommand)]
  command: Option<Command>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum VerificationMode {
  Offline,
  Hardware,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
  /// Inspect watt's configuration.
  Config {
    #[command(subcommand)]
    command: ConfigCommand,
  },
}

#[derive(clap::Subcommand, Debug)]
enum ConfigCommand {
  /// Print the active rules selected by the daemon.
  Active {
    /// Continue printing the active rules whenever they change.
    #[arg(long)]
    watch: bool,
  },

  /// Print configuration as normalized TOML.
  Show {
    /// Show the built-in default instead of the running daemon's config.
    #[arg(long)]
    default: bool,
  },

  /// Print the configuration's JSON Schema.
  Schema,
}

pub fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();

  yansi::whenever(yansi::Condition::TTY_AND_COLOR);

  env_logger::Builder::new()
    .filter_level(cli.verbosity.log_level_filter())
    .format_timestamp(None)
    .format_module_path(false)
    .init();

  if cli.verify_config.is_some() && cli.command.is_some() {
    bail!("--verify-config cannot be combined with a subcommand");
  }
  if let Some(Command::Config { command }) = cli.command {
    return run_config_command(command);
  }

  let config_path = cli.config.clone();
  let config = config::DaemonConfig::load_from(config_path.as_deref())
    .context("failed to load daemon config")?;

  if let Some(mode) = cli.verify_config {
    return verify_configuration(&config, mode);
  }

  log::info!("starting watt daemon");

  let lock_path = fs::path("/run/watt/lock");
  let _lock = lock::LockFile::acquire(&lock_path)?;

  let runtime = RuntimeBuilder::new_multi_thread()
    .enable_all()
    .build()
    .context("failed to build tokio runtime")?;

  runtime.block_on(system::run_daemon(config, config_path))
}

fn verify_configuration(
  config: &config::DaemonConfig,
  mode: VerificationMode,
) -> anyhow::Result<()> {
  if let VerificationMode::Hardware = mode {
    for diagnostic in config.verify_hardware()? {
      log::warn!("{diagnostic}");
    }
  }
  print_output("Configuration valid.")
}

fn run_config_command(command: ConfigCommand) -> anyhow::Result<()> {
  match command {
    ConfigCommand::Active { watch } => {
      let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
      runtime.block_on(dbus::client::print_active_rules(watch))
    },
    ConfigCommand::Show { default } => {
      if default {
        let config = config::DaemonConfig::load_from(None)
          .context("failed to load default config")?;
        let output = toml::to_string_pretty(&config)
          .context("failed to serialize config")?;
        print_output(&output)
      } else {
        let runtime = RuntimeBuilder::new_current_thread()
          .enable_all()
          .build()
          .context("failed to build tokio runtime")?;
        let output = runtime.block_on(dbus::client::loaded_config())?;
        print_output(&output)
      }
    },
    ConfigCommand::Schema => {
      let schema = schemars::schema_for!(config::DaemonConfig);
      let output = serde_json::to_string_pretty(&schema)
        .context("failed to serialize config schema")?;
      print_output(&output)
    },
  }
}

fn print_output(output: &str) -> anyhow::Result<()> {
  let stdout = io::stdout();
  let mut stdout = stdout.lock();
  writeln!(stdout, "{output}")?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use clap::Parser;

  use super::*;

  #[test]
  fn parses_config_commands() {
    let cli =
      Cli::try_parse_from(["watt", "config", "show", "--default"]).unwrap();
    assert!(matches!(
      cli.command,
      Some(Command::Config {
        command: ConfigCommand::Show { default: true },
      })
    ));

    let cli =
      Cli::try_parse_from(["watt", "config", "active", "--watch"]).unwrap();
    assert!(matches!(
      cli.command,
      Some(Command::Config {
        command: ConfigCommand::Active { watch: true },
      })
    ));
    assert!(
      Cli::try_parse_from([
        "watt",
        "config",
        "show",
        "--config",
        "custom.toml",
      ])
      .is_err()
    );
  }

  #[test]
  fn generated_schema_is_json_schema() {
    let schema = schemars::schema_for!(config::DaemonConfig);
    let value = serde_json::to_value(schema).unwrap();

    assert_eq!(
      value["$schema"],
      "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(value["properties"]["rule"].is_object());
    assert!(value["$defs"]["Expression"].is_object());
  }

  #[test]
  fn normalized_default_config_round_trips() {
    let config = config::DaemonConfig::load_from(None).unwrap();
    let output = toml::to_string_pretty(&config).unwrap();
    let reparsed: config::DaemonConfig = toml::from_str(&output).unwrap();

    assert_eq!(reparsed, config);
  }
}
