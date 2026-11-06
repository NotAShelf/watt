use std::{
  fmt,
  fs::DirEntry,
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

pub type UsbInventory = fs::Inventory<UsbDevice>;

impl UsbInventory {
  pub fn scan() -> anyhow::Result<Self> {
    let entries = fs::read_dir("/sys/bus/usb/devices")?
      .context("USB inventory directory is unavailable")?;
    Ok(Self::scan_entries(entries, "USB", UsbDevice::scan))
  }
}

impl UsbDevice {
  pub fn all() -> anyhow::Result<Vec<Self>> {
    UsbInventory::scan()?.into_devices("USB")
  }

  fn scan(entry: DirEntry) -> anyhow::Result<Option<Self>> {
    let name = entry.file_name().to_string_lossy().into_owned();
    // Entries like "1-1:1.0" are interfaces, not devices.
    if name.contains(':') {
      return Ok(None);
    }
    let path = fs::canonicalize(entry.path())?.with_context(|| {
      format!("USB device {name:?} disappeared during discovery")
    })?;
    fs::read(path.join("idVendor"))?.with_context(|| {
      format!("USB device {name:?} vendor capability is unavailable")
    })?;
    Ok(Some(Self { name, path }))
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

#[cfg(test)]
mod tests {
  use std::{
    fs as stdfs,
    os::unix::fs::symlink,
  };

  use super::*;
  use crate::test_support::SystemFixture;

  #[test]
  fn aliases_share_the_original_autosuspend_setting() {
    let fixture = SystemFixture::new();
    let device = fixture.path("/sys/devices/usb1/1-1");
    fixture.write(device.join("idVendor"), "1234");
    let control = device.join("power/control");
    fixture.write(&control, "on");
    let aliases = fixture.path("/sys/bus/usb/devices");
    stdfs::create_dir_all(&aliases).unwrap();
    symlink(&device, aliases.join("1-1")).unwrap();
    symlink(&device, aliases.join("renamed")).unwrap();
    let devices = UsbDevice::all().unwrap();
    assert_eq!(devices.len(), 2);
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    for device in devices {
      device.set_autosuspend(true).unwrap();
    }
    assert_eq!(fs::applied_settings().len(), 1);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(control).unwrap(), "on");
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
