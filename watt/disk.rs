use std::{
  fmt,
  hash,
  path::PathBuf,
  process::Command,
};

use anyhow::{
  Context,
  bail,
};
use yansi::Paint as _;

use crate::fs;

const BLOCK_PATH: &str = "/sys/block";

#[derive(Debug, Clone)]
pub struct Disk {
  pub name: String,
  pub path: PathBuf,
}

impl PartialEq for Disk {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name
  }
}

impl Eq for Disk {}

impl hash::Hash for Disk {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl fmt::Display for Disk {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "disk '{name}'", name = self.name.cyan())
  }
}

impl Disk {
  pub fn all() -> anyhow::Result<Vec<Self>> {
    let Some(entries) = fs::read_dir(BLOCK_PATH)? else {
      return Ok(Vec::new());
    };

    let mut disks = Vec::new();

    for entry in entries {
      let entry = entry.context("failed to read block device entry")?;
      let path = entry.path();
      let name = entry.file_name().to_string_lossy().to_string();

      if !path.join("queue").exists() || name.starts_with("loop") {
        continue;
      }
      if fs::read_n::<u64>(path.join("removable"))?.unwrap_or(0) == 1 {
        continue;
      }

      disks.push(Self { name, path });
    }

    Ok(disks)
  }

  pub fn set_scheduler(&self, value: &str) -> anyhow::Result<()> {
    fs::write(self.path.join("queue/scheduler"), value)
      .with_context(|| format!("failed to set scheduler for {self}"))
  }

  pub fn set_readahead_kib(&self, value: u64) -> anyhow::Result<()> {
    fs::write(self.path.join("queue/read_ahead_kb"), &value.to_string())
      .with_context(|| format!("failed to set readahead for {self}"))
  }

  pub fn set_apm(&self, value: u8) -> anyhow::Result<()> {
    run_hdparm(self, format!("-B{value}"))
  }

  pub fn set_spindown(&self, value: u8) -> anyhow::Result<()> {
    run_hdparm(self, format!("-S{value}"))
  }
}

fn run_hdparm(disk: &Disk, option: String) -> anyhow::Result<()> {
  let device = format!("/dev/{name}", name = disk.name);
  let status = Command::new("hdparm")
    .arg(option)
    .arg(&device)
    .status()
    .with_context(|| format!("failed to execute hdparm for {disk}"))?;

  if !status.success() {
    bail!("hdparm failed for {disk} with status {status}");
  }

  Ok(())
}

pub fn set_alpm(policy: &str) -> anyhow::Result<()> {
  let Some(entries) = fs::read_dir("/sys/class/scsi_host")? else {
    return Ok(());
  };

  for entry in entries {
    let entry = entry.context("failed to read SCSI host entry")?;
    let host_path = entry.path();
    let policy_path = host_path.join("link_power_management_policy");

    if is_external_sata_port(&host_path)? {
      log::info!(
        "skipping ALPM for external or hotplug-capable SATA host '{host}'",
        host = host_path.display(),
      );
      continue;
    }

    if policy_path.exists() {
      fs::write(&policy_path, policy).with_context(|| {
        format!(
          "failed to set ALPM policy at '{path}'",
          path = policy_path.display()
        )
      })?;
    }
  }

  Ok(())
}

fn is_external_sata_port(host_path: &std::path::Path) -> anyhow::Result<bool> {
  let Some(value) = fs::read(host_path.join("ahci_port_cmd"))? else {
    return Ok(false);
  };
  let value = value.trim_start_matches("0x");
  let port_cmd = u64::from_str_radix(value, 16).with_context(|| {
    format!(
      "failed to parse AHCI port command for '{host}'",
      host = host_path.display()
    )
  })?;

  const HOTPLUG_CAPABLE_PORT: u64 = 1 << 18;
  const EXTERNAL_SATA_PORT: u64 = 1 << 21;

  Ok(port_cmd & (HOTPLUG_CAPABLE_PORT | EXTERNAL_SATA_PORT) != 0)
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub scheduler:     Option<String>,
  pub readahead_kib: Option<u64>,
  pub apm:           Option<u8>,
  pub spindown:      Option<u8>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.scheduler.is_some()
      && self.readahead_kib.is_some()
      && self.apm.is_some()
      && self.spindown.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      scheduler:     self.scheduler.or_else(|| that.scheduler.clone()),
      readahead_kib: self.readahead_kib.or(that.readahead_kib),
      apm:           self.apm.or(that.apm),
      spindown:      self.spindown.or(that.spindown),
    }
  }

  pub fn apply(&self, disk: &Disk) -> anyhow::Result<()> {
    if let Some(value) = &self.scheduler {
      disk.set_scheduler(value)?;
    }
    if let Some(value) = self.readahead_kib {
      disk.set_readahead_kib(value)?;
    }
    if let Some(value) = self.apm {
      disk.set_apm(value)?;
    }
    if let Some(value) = self.spindown {
      disk.set_spindown(value)?;
    }

    Ok(())
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct GlobalDelta {
  pub alpm: Option<String>,
}

impl GlobalDelta {
  pub fn is_some(&self) -> bool {
    self.alpm.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      alpm: self.alpm.or_else(|| that.alpm.clone()),
    }
  }

  pub fn apply(&self) -> anyhow::Result<()> {
    if let Some(value) = &self.alpm {
      set_alpm(value)?;
    }

    Ok(())
  }
}
