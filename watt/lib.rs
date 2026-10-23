use std::{
  fs::File,
  io::{
    self,
    Read as _,
    Write,
  },
  path::{
    Path,
    PathBuf,
  },
};

use anyhow::{
  Context as _,
  bail,
  ensure,
};
use clap::Parser as _;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::dbus::client::OutputFormat;

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

  /// Print daemon query results as stable JSON.
  #[arg(long, global = true)]
  json: bool,

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
  /// Ask the running daemon to verify a local configuration file.
  Verify {
    path:     PathBuf,
    /// Check controls on the daemon's host as well as static configuration.
    #[arg(long)]
    hardware: bool,
  },
  /// Print the active rules selected by the daemon.
  Active {
    /// Continue printing the active rules whenever they change.
    #[arg(long)]
    watch: bool,
  },
  /// Ask the running daemon to reload its file-backed configuration.
  Reload,
  #[command(flatten)]
  Query(ConfigQuery),
  /// Read or select the active power profile.
  Profile {
    #[command(subcommand)]
    command: ProfileCommand,
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

impl ConfigCommand {
  fn validate_format(&self, format: OutputFormat) -> anyhow::Result<()> {
    let json_query = matches!(
      self,
      Self::Query(_)
        | Self::Active { .. }
        | Self::Profile {
          command: ProfileCommand::Get,
        }
    );
    ensure!(
      format != OutputFormat::Json || json_query,
      "--json is supported only for daemon queries"
    );
    Ok(())
  }
}

#[derive(clap::Subcommand, Debug)]
enum ConfigQuery {
  /// Print recent daemon errors.
  Errors,
  /// Print the daemon's current status.
  Status,
  /// Print configured rules and priorities.
  Rules,
  /// Print discovered CPU and device capabilities.
  Capabilities,
  /// Explain applied settings and their winning rules.
  Settings,
  /// Explain adaptive polling decisions and sample availability.
  Polling,
}

#[derive(clap::Subcommand, Debug)]
enum ProfileCommand {
  Get,
  Set { profile: String },
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
    return run_config_command(command, OutputFormat::from(cli.json));
  }

  ensure!(!cli.json, "--json requires a config query subcommand");
  run_daemon_command(cli.config, cli.verify_config)
}

fn run_daemon_command(
  config_path: Option<PathBuf>,
  verification: Option<VerificationMode>,
) -> anyhow::Result<()> {
  let config = config::DaemonConfig::load_from(config_path.as_deref())
    .context("failed to load daemon config")?;

  if let Some(mode) = verification {
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
    config::verify::report(config.verify_hardware()?)?;
  }
  print_output("Configuration valid.")
}

fn run_config_command(
  command: ConfigCommand,
  format: OutputFormat,
) -> anyhow::Result<()> {
  command.validate_format(format)?;
  let runtime = RuntimeBuilder::new_current_thread()
    .enable_all()
    .build()
    .context("failed to build tokio runtime")?;
  match command {
    ConfigCommand::Verify { path, hardware } => {
      let contents = read_verification_input(&path)?;
      runtime.block_on(dbus::client::verify_configuration(&contents, hardware))
    },
    ConfigCommand::Active { watch } => {
      runtime.block_on(dbus::client::print_active_rules(watch, format))
    },
    ConfigCommand::Reload => runtime.block_on(dbus::client::reload()),
    ConfigCommand::Query(query) => runtime.block_on(run_query(query, format)),
    ConfigCommand::Profile { command } => {
      runtime.block_on(run_profile(command, format))
    },
    ConfigCommand::Show { default } => {
      runtime.block_on(show_configuration(default))
    },
    ConfigCommand::Schema => print_schema(),
  }
}

async fn run_query(
  query: ConfigQuery,
  format: OutputFormat,
) -> anyhow::Result<()> {
  match query {
    ConfigQuery::Errors => dbus::client::print_errors(format).await,
    ConfigQuery::Status => dbus::client::print_status(format).await,
    ConfigQuery::Rules => dbus::client::print_configured_rules(format).await,
    ConfigQuery::Capabilities => dbus::client::print_capabilities(format).await,
    ConfigQuery::Settings => dbus::client::print_settings(format).await,
    ConfigQuery::Polling => dbus::client::print_polling(format).await,
  }
}

async fn run_profile(
  command: ProfileCommand,
  format: OutputFormat,
) -> anyhow::Result<()> {
  match command {
    ProfileCommand::Get => dbus::client::get_profile(format).await,
    ProfileCommand::Set { profile } => {
      dbus::client::set_profile(&profile).await
    },
  }
}

fn print_schema() -> anyhow::Result<()> {
  let schema = schemars::schema_for!(config::DaemonConfig);
  let output = serde_json::to_string_pretty(&schema)
    .context("failed to serialize config schema")?;
  print_output(&output)
}

async fn show_configuration(default: bool) -> anyhow::Result<()> {
  let output = if default {
    let config = config::DaemonConfig::load_from(None)
      .context("failed to load default config")?;
    toml::to_string_pretty(&config).context("failed to serialize config")?
  } else {
    dbus::client::loaded_config().await?
  };
  print_output(&output)
}

fn read_verification_input(path: &Path) -> anyhow::Result<String> {
  const MAX_BYTES: u64 = 1024 * 1024;

  let mut contents = String::new();
  File::open(path)
    .with_context(|| format!("failed to open {}", path.display()))?
    .take(MAX_BYTES + 1)
    .read_to_string(&mut contents)
    .with_context(|| format!("failed to read {}", path.display()))?;
  ensure!(
    contents.len() as u64 <= MAX_BYTES,
    "configuration exceeds 1 MiB"
  );
  Ok(contents)
}

fn print_output(output: &str) -> anyhow::Result<()> {
  let stdout = io::stdout();
  let mut stdout = stdout.lock();
  writeln!(stdout, "{output}")?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

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
