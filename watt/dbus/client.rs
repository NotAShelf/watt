use std::io::{
  self,
  Write,
};

use anyhow::Context;
use futures_util::StreamExt;

#[zbus::proxy(
  interface = "dev.notashelf.Watt",
  default_service = "dev.notashelf.Watt",
  default_path = "/dev/notashelf/Watt"
)]
trait Watt {
  fn get_applied_rules(&self) -> zbus::Result<Vec<String>>;
  fn get_config(&self) -> zbus::Result<String>;

  #[zbus(signal)]
  fn applied_rules_changed(&self) -> zbus::Result<()>;
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
