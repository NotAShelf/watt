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

use config::verify::report as report_verification;
use misstep::{
  ResultExt as _,
  bail,
  ensure,
};
use pound::Parse as _;
use tokio::runtime::Builder as RuntimeBuilder;
pub use watt_config as config;
pub use watt_platform::{
  audio,
  cpu,
  disk,
  fs,
  gpu,
  power_supply,
  uncore,
  usb,
  vm,
};

use crate::dbus::client::OutputFormat;

pub mod system;

pub mod lock;

pub mod dbus;
#[cfg(feature = "metrics")] pub mod metrics;
pub mod profile;

#[cfg(test)] pub(crate) use watt_platform::test_support;

/// Modern CPU frequency and power management utility for Linux.
#[derive(pound::Parse, Debug)]
#[pound(name = "watt")]
pub struct Cli {
  #[pound(flatten)]
  verbosity: Verbosity,

  /// The daemon config path.
  #[pound(long, env = "WATT_CONFIG")]
  config: Option<PathBuf>,

  /// Print daemon query results as stable JSON.
  #[pound(long, global)]
  json: bool,

  /// Check configuration and exit. Hardware mode also checks discovered CPU,
  /// power-supply and peripheral controls.
  #[pound(long, default_missing = "offline")]
  verify_config: Option<VerificationMode>,

  #[pound(subcommand)]
  command: Option<Command>,
}

#[derive(pound::Parse, Debug)]
struct Verbosity {
  /// Increase logging verbosity. Repeat for trace logging.
  #[pound(short, long, count, conflicts_with = quiet)]
  verbose: u8,

  /// Reduce logging verbosity. Repeat to disable logging.
  #[pound(short, long, count)]
  quiet: u8,
}

impl Verbosity {
  fn log_level_filter(&self) -> log::LevelFilter {
    match (self.verbose, self.quiet) {
      (0, 0) => log::LevelFilter::Info,
      (0, 1) => log::LevelFilter::Warn,
      (0, 2) => log::LevelFilter::Error,
      (0, _) => log::LevelFilter::Off,
      (1, 0) => log::LevelFilter::Debug,
      (_, 0) => log::LevelFilter::Trace,
      _ => unreachable!("pound rejects mixed verbosity flags"),
    }
  }
}

#[derive(pound::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum VerificationMode {
  Offline,
  Hardware,
}

#[derive(pound::Parse, Debug)]
enum Command {
  /// Inspect watt's configuration.
  Config {
    #[pound(subcommand)]
    command: ConfigCommand,
  },
}

#[derive(pound::Parse, Debug)]
enum ConfigCommand {
  /// Ask the running daemon to verify a local configuration file.
  Verify {
    path:     PathBuf,
    /// Check controls on the daemon's host as well as static configuration.
    #[pound(long)]
    hardware: bool,
  },
  /// Print the active rules selected by the daemon.
  Active {
    /// Continue printing the active rules whenever they change.
    #[pound(long)]
    watch: bool,
  },
  /// Ask the running daemon to reload its file-backed configuration.
  Reload,
  #[pound(flatten)]
  Query(ConfigQuery),
  /// Read or select the active power profile.
  Profile {
    #[pound(subcommand)]
    command: ProfileCommand,
  },

  /// Print configuration as normalized TOML.
  Show {
    /// Show the built-in default instead of the running daemon's config.
    #[pound(long)]
    default: bool,
  },

  /// Print the configuration's JSON Schema.
  Schema,
}

impl ConfigCommand {
  fn validate_format(&self, format: OutputFormat) -> misstep::Result<()> {
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

#[derive(pound::Parse, Debug)]
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

#[derive(pound::Parse, Debug)]
enum ProfileCommand {
  Get,
  Set { profile: String },
}

pub fn main() -> misstep::Result<()> {
  caress::handle(Cli::SPEC);
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
) -> misstep::Result<()> {
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
) -> misstep::Result<()> {
  if let VerificationMode::Hardware = mode {
    report_verification(config.verify_hardware()?)?;
  }
  print_output("Configuration valid.")
}

fn run_config_command(
  command: ConfigCommand,
  format: OutputFormat,
) -> misstep::Result<()> {
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
) -> misstep::Result<()> {
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
) -> misstep::Result<()> {
  match command {
    ProfileCommand::Get => dbus::client::get_profile(format).await,
    ProfileCommand::Set { profile } => {
      dbus::client::set_profile(&profile).await
    },
  }
}

fn print_schema() -> misstep::Result<()> {
  let schema = schemars::schema_for!(config::DaemonConfig);
  let output = serde_json::to_string_pretty(&schema)
    .context("failed to serialize config schema")?;
  print_output(&output)
}

async fn show_configuration(default: bool) -> misstep::Result<()> {
  let output = if default {
    let config = config::DaemonConfig::load_from(None)
      .context("failed to load default config")?;
    toml::to_string_pretty(&config).context("failed to serialize config")?
  } else {
    dbus::client::loaded_config().await?
  };
  print_output(&output)
}

fn read_verification_input(path: &Path) -> misstep::Result<String> {
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

fn print_output(output: &str) -> misstep::Result<()> {
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
