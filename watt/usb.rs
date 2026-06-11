use std::{
  fmt,
  hash,
  path::PathBuf,
};

use anyhow::Context;
use yansi::Paint as _;

use crate::fs;

#[derive(Debug, Clone)]
pub struct UsbDevice {
  pub name: String,
  pub path: PathBuf,
}

impl PartialEq for UsbDevice {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name
  }
}

impl Eq for UsbDevice {}

impl hash::Hash for UsbDevice {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl fmt::Display for UsbDevice {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "USB device '{name}'", name = self.name.cyan())
  }
}

impl UsbDevice {
  pub fn all() -> anyhow::Result<Vec<Self>> {
    let Some(entries) = fs::read_dir("/sys/bus/usb/devices")? else {
      return Ok(Vec::new());
    };

    let mut devices = Vec::new();

    for entry in entries {
      let entry = entry.context("failed to read USB device entry")?;
      let path = entry.path();
      if !path.join("power").exists() || !path.join("idVendor").exists() {
        continue;
      }

      devices.push(Self {
        name: entry.file_name().to_string_lossy().to_string(),
        path,
      });
    }

    Ok(devices)
  }

  pub fn set_autosuspend(&self, enabled: bool) -> anyhow::Result<()> {
    let value = if enabled { "auto" } else { "on" };
    fs::write(self.path.join("power/control"), value)
      .with_context(|| format!("failed to set autosuspend for {self}"))
  }

  pub fn set_autosuspend_delay_ms(&self, delay_ms: u64) -> anyhow::Result<()> {
    fs::write(
      self.path.join("power/autosuspend_delay_ms"),
      &delay_ms.to_string(),
    )
    .with_context(|| format!("failed to set autosuspend delay for {self}"))
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub autosuspend:          Option<bool>,
  pub autosuspend_delay_ms: Option<u64>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.autosuspend.is_some() && self.autosuspend_delay_ms.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      autosuspend:          self.autosuspend.or(that.autosuspend),
      autosuspend_delay_ms: self
        .autosuspend_delay_ms
        .or(that.autosuspend_delay_ms),
    }
  }

  pub fn apply(&self, device: &UsbDevice) -> anyhow::Result<()> {
    if let Some(enabled) = self.autosuspend {
      device.set_autosuspend(enabled)?;
    }
    if let Some(delay_ms) = self.autosuspend_delay_ms {
      device.set_autosuspend_delay_ms(delay_ms)?;
    }

    Ok(())
  }
}
