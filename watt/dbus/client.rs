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
use zbus::{
  Connection,
  zvariant::OwnedValue,
};

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

pub async fn print_errors() -> anyhow::Result<()> {
  for error in watt()
    .await?
    .get_recent_errors()
    .await
    .context("failed to get daemon errors")?
  {
    println!("{error}");
  }
  Ok(())
}

pub async fn print_status() -> anyhow::Result<()> {
  for (name, value) in watt()
    .await?
    .get_status()
    .await
    .context("failed to get daemon status")?
  {
    println!("{name}: {value:?}");
  }
  Ok(())
}

pub async fn print_configured_rules() -> anyhow::Result<()> {
  for (name, priority) in watt()
    .await?
    .get_rules()
    .await
    .context("failed to get daemon rules")?
  {
    println!("{priority}\t{name}");
  }
  Ok(())
}

pub async fn print_capabilities() -> anyhow::Result<()> {
  for capability in watt()
    .await?
    .get_cpu_capabilities()
    .await
    .context("failed to get CPU capabilities")?
  {
    println!("{capability:?}");
  }
  Ok(())
}

pub async fn get_profile() -> anyhow::Result<()> {
  let profile = PowerProfilesProxy::new(&connect().await?)
    .await?
    .active_profile()
    .await
    .context("failed to get active profile")?;
  println!("{profile}");
  Ok(())
}

pub async fn set_profile(profile: &str) -> anyhow::Result<()> {
  PowerProfilesProxy::new(&connect().await?)
    .await?
    .set_active_profile(profile)
    .await?;
  Ok(())
}

pub async fn print_active_rules(watch: bool) -> anyhow::Result<()> {
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
  print_rules(&rules, false)?;

  let Some(signals) = &mut signals else {
    return Ok(());
  };

  while signals.next().await.is_some() {
    let next = proxy
      .get_applied_rules()
      .await
      .context("failed to get active rules")?;
    if next != rules {
      print_rules(&next, true)?;
      rules = next;
    }
  }

  anyhow::bail!("connection to the watt daemon closed")
}

fn print_rules(rules: &[String], separator: bool) -> anyhow::Result<()> {
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
