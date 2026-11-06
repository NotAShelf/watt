use std::{
  collections::HashMap,
  io::{
    self,
    Write,
  },
};

use anyhow::{
  Context,
  ensure,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{
  Value,
  json,
};
use zbus::{
  Connection,
  zvariant::OwnedValue,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
  Human,
  Json,
}

impl From<bool> for OutputFormat {
  fn from(json: bool) -> Self {
    if json { Self::Json } else { Self::Human }
  }
}

#[zbus::proxy(
  interface = "dev.notashelf.Watt",
  default_service = "dev.notashelf.Watt",
  default_path = "/dev/notashelf/Watt"
)]
trait Watt {
  fn verify_configuration(
    &self,
    contents: &str,
    hardware: bool,
  ) -> zbus::Result<Vec<(String, String, u16, String, String)>>;
  fn get_applied_rules(&self) -> zbus::Result<Vec<String>>;
  fn get_config(&self) -> zbus::Result<String>;
  fn reload(&self) -> zbus::Result<()>;
  fn get_recent_errors(&self) -> zbus::Result<Vec<String>>;
  fn get_status(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
  fn get_rules(&self) -> zbus::Result<Vec<(String, u16)>>;
  fn get_cpu_capabilities(
    &self,
  ) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
  fn get_device_capabilities(
    &self,
  ) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
  fn get_applied_setting_details(
    &self,
  ) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
  fn get_polling_diagnostics(
    &self,
  ) -> zbus::Result<HashMap<String, OwnedValue>>;

  #[zbus(signal)]
  fn applied_rules_changed(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
  interface = "net.hadess.PowerProfiles",
  default_service = "net.hadess.PowerProfiles",
  default_path = "/net/hadess/PowerProfiles"
)]
trait PowerProfiles {
  #[zbus(property)]
  fn active_profile(&self) -> zbus::Result<String>;
  #[zbus(property)]
  fn set_active_profile(&self, profile: &str) -> zbus::Result<()>;
}

async fn connect() -> anyhow::Result<Connection> {
  Connection::system()
    .await
    .context("failed to connect to the system D-Bus")
}

async fn watt() -> anyhow::Result<WattProxy<'static>> {
  WattProxy::new(&connect().await?)
    .await
    .context("failed to connect to the watt daemon")
}

pub async fn verify_configuration(
  contents: &str,
  hardware: bool,
) -> anyhow::Result<()> {
  let diagnostics = watt()
    .await?
    .verify_configuration(contents, hardware)
    .await
    .context("daemon configuration verification failed")?;
  let mut invalid = false;
  for (severity, rule, priority, field, message) in diagnostics {
    eprintln!(
      "{severity}: rule {rule:?} (priority {priority}), {field}: {message}"
    );
    invalid |= severity == "error";
  }
  ensure!(!invalid, "configuration verification found errors");
  Ok(())
}

pub async fn loaded_config() -> anyhow::Result<String> {
  let connection = zbus::Connection::system()
    .await
    .context("failed to connect to the system D-Bus")?;
  let proxy = WattProxy::new(&connection)
    .await
    .context("failed to connect to the watt daemon")?;

  proxy
    .get_config()
    .await
    .context("failed to get the loaded config")
}

pub async fn reload() -> anyhow::Result<()> {
  watt()
    .await?
    .reload()
    .await
    .context("daemon configuration reload failed")
}

pub async fn print_errors(format: OutputFormat) -> anyhow::Result<()> {
  let errors = watt()
    .await?
    .get_recent_errors()
    .await
    .context("failed to get daemon errors")?;
  if format == OutputFormat::Json {
    return print_payload(errors, format);
  }
  let mut stdout = io::stdout().lock();
  for error in errors {
    writeln!(stdout, "{error}")?;
  }
  Ok(())
}

pub async fn print_status(format: OutputFormat) -> anyhow::Result<()> {
  let status = watt()
    .await?
    .get_status()
    .await
    .context("failed to get daemon status")?;
  print_payload(status, format)
}

pub async fn print_configured_rules(
  format: OutputFormat,
) -> anyhow::Result<()> {
  let rules = watt()
    .await?
    .get_rules()
    .await
    .context("failed to get daemon rules")?;
  if format == OutputFormat::Json {
    let rules: Vec<_> = rules
      .into_iter()
      .map(|(name, priority)| json!({"name": name, "priority": priority}))
      .collect();
    return print_payload(rules, format);
  }
  let mut stdout = io::stdout().lock();
  for (name, priority) in rules {
    writeln!(stdout, "{priority}\t{name}")?;
  }
  Ok(())
}

pub async fn print_capabilities(format: OutputFormat) -> anyhow::Result<()> {
  let proxy = watt().await?;
  let cpus = proxy
    .get_cpu_capabilities()
    .await
    .context("failed to get CPU capabilities")?;
  let devices = proxy
    .get_device_capabilities()
    .await
    .context("failed to get device capabilities")?;
  print_payload(json!({"cpus": cpus, "devices": devices}), format)
}

pub async fn print_settings(format: OutputFormat) -> anyhow::Result<()> {
  let settings = watt()
    .await?
    .get_applied_setting_details()
    .await
    .context("failed to get applied settings")?;
  print_payload(settings, format)
}

pub async fn print_polling(format: OutputFormat) -> anyhow::Result<()> {
  let polling = watt()
    .await?
    .get_polling_diagnostics()
    .await
    .context("failed to get polling diagnostics")?;
  print_payload(polling, format)
}

pub async fn get_profile(format: OutputFormat) -> anyhow::Result<()> {
  let profile = PowerProfilesProxy::new(&connect().await?)
    .await?
    .active_profile()
    .await
    .context("failed to get active profile")?;
  if format == OutputFormat::Json {
    return print_payload(json!({"profile": profile}), format);
  }
  writeln!(io::stdout().lock(), "{profile}")?;
  Ok(())
}

pub async fn set_profile(profile: &str) -> anyhow::Result<()> {
  PowerProfilesProxy::new(&connect().await?)
    .await?
    .set_active_profile(profile)
    .await?;
  Ok(())
}

fn print_payload(
  payload: impl Serialize,
  format: OutputFormat,
) -> anyhow::Result<()> {
  let value = plain_variants(serde_json::to_value(payload)?);
  let mut stdout = io::stdout().lock();
  match format {
    OutputFormat::Human => serde_json::to_writer_pretty(&mut stdout, &value)?,
    OutputFormat::Json => serde_json::to_writer(&mut stdout, &value)?,
  }
  writeln!(stdout)?;
  Ok(())
}

/// Unwraps serialized D-Bus variants (`{"signature", "value"}`) into their
/// plain values.
fn plain_variants(value: Value) -> Value {
  match value {
    Value::Object(mut object) => {
      if object.len() == 2
        && object.get("signature").is_some_and(Value::is_string)
        && let Some(value) = object.remove("value")
      {
        return plain_variants(value);
      }
      Value::Object(
        object
          .into_iter()
          .map(|(key, value)| (key, plain_variants(value)))
          .collect(),
      )
    },
    Value::Array(values) => {
      Value::Array(values.into_iter().map(plain_variants).collect())
    },
    value => value,
  }
}

pub async fn print_active_rules(
  watch: bool,
  format: OutputFormat,
) -> anyhow::Result<()> {
  let connection = zbus::Connection::system()
    .await
    .context("failed to connect to the system D-Bus")?;
  let proxy = WattProxy::new(&connection)
    .await
    .context("failed to connect to the watt daemon")?;

  let mut signals = if watch {
    Some(
      proxy
        .receive_applied_rules_changed()
        .await
        .context("failed to watch active rules")?,
    )
  } else {
    None
  };
  let mut rules = proxy
    .get_applied_rules()
    .await
    .context("failed to get active rules")?;
  print_rules(&rules, false, format)?;

  let Some(signals) = &mut signals else {
    return Ok(());
  };

  while signals.next().await.is_some() {
    let next = proxy
      .get_applied_rules()
      .await
      .context("failed to get active rules")?;
    if next != rules {
      print_rules(&next, true, format)?;
      rules = next;
    }
  }

  anyhow::bail!("connection to the watt daemon closed")
}

fn print_rules(
  rules: &[String],
  separator: bool,
  format: OutputFormat,
) -> anyhow::Result<()> {
  if format == OutputFormat::Json {
    return print_payload(rules, format);
  }
  let stdout = io::stdout();
  let mut stdout = stdout.lock();

  if separator {
    writeln!(stdout, "--")?;
  }
  for rule in rules {
    writeln!(stdout, "{rule}")?;
  }
  stdout.flush()?;

  Ok(())
}
