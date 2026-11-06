use std::{
  fmt,
  fs::DirEntry,
  hash,
  path::{
    Path,
    PathBuf,
  },
  process::Command,
};

use misstep::{
  OptionExt as _,
  ResultExt as _,
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

pub type DiskInventory = fs::Inventory<Disk>;

impl DiskInventory {
  pub fn scan() -> misstep::Result<Self> {
    let entries = fs::read_dir(BLOCK_PATH)?
      .context("block-device inventory directory is unavailable")?;
    Ok(Self::scan_entries(entries, "disk", Disk::scan))
  }
}

impl Disk {
  pub fn all() -> misstep::Result<Vec<Self>> {
    DiskInventory::scan()?.into_devices("disk")
  }

  fn scan(entry: DirEntry) -> misstep::Result<Option<Self>> {
    let name = entry.file_name().to_string_lossy().into_owned();
    if name.starts_with("loop") {
      return Ok(None);
    }
    let path = fs::canonicalize(entry.path())?
      .with_context(|| format!("disk {name:?} disappeared during discovery"))?;
    let removable =
      fs::read_n::<u64>(path.join("removable"))?.with_context(|| {
        format!("disk {name:?} removable capability is unavailable")
      })?;
    if removable == 1 {
      return Ok(None);
    }
    Ok(Some(Self { name, path }))
  }

  pub fn set_scheduler(&self, value: &str) -> misstep::Result<()> {
    fs::write(self.path.join("queue/scheduler"), value)
      .with_context(|| format!("failed to set scheduler for {self}"))
  }

  pub fn set_readahead_kib(&self, value: u64) -> misstep::Result<()> {
    fs::write(self.path.join("queue/read_ahead_kb"), &value.to_string())
      .with_context(|| format!("failed to set readahead for {self}"))
  }

  pub fn set_apm(&self, value: u8) -> misstep::Result<()> {
    run_hdparm(self, format!("-B{value}"))
  }

  pub fn set_spindown(&self, value: u8) -> misstep::Result<()> {
    run_hdparm(self, format!("-S{value}"))
  }
}

fn run_hdparm(disk: &Disk, option: String) -> misstep::Result<()> {
  let device = fs::path(format!("/dev/{name}", name = disk.name));
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

/// Returns the canonical policy controls for internal, non-hotplug SATA hosts.
pub fn alpm_paths() -> misstep::Result<Vec<PathBuf>> {
  let Some(entries) = fs::read_dir("/sys/class/scsi_host")? else {
    return Ok(Vec::new());
  };

  let mut paths = Vec::new();
  for entry in entries {
    let entry = entry.context("failed to read SCSI host entry")?;
    let Some(host_path) = fs::canonicalize(entry.path())? else {
      continue;
    };
    let Some(policy_path) =
      fs::canonicalize(host_path.join("link_power_management_policy"))?
    else {
      continue;
    };

    if is_external_sata_port(&host_path)? {
      log::trace!(
        "skipping ALPM for external or hotplug-capable SATA host '{host}'",
        host = host_path.display(),
      );
      continue;
    }
    paths.push(policy_path);
  }

  paths.sort_unstable();
  paths.dedup();
  Ok(paths)
}

pub fn set_alpm(policy: &str) -> misstep::Result<()> {
  for path in alpm_paths()? {
    fs::write(&path, policy).with_context(|| {
      format!("failed to set ALPM policy at '{}'", path.display())
    })?;
  }

  Ok(())
}

fn is_external_sata_port(host_path: &Path) -> misstep::Result<bool> {
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
  pub scheduler:     Option<fs::Setting<String>>,
  pub readahead_kib: Option<fs::Setting<u64>>,
  pub apm:           Option<fs::Setting<u8>>,
  pub spindown:      Option<fs::Setting<u8>>,
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
      scheduler:     fs::prefer(self.scheduler, &that.scheduler),
      readahead_kib: fs::prefer(self.readahead_kib, &that.readahead_kib),
      apm:           fs::prefer(self.apm, &that.apm),
      spindown:      fs::prefer(self.spindown, &that.spindown),
    }
  }

  pub fn apply(&self, disk: &Disk) -> misstep::Result<()> {
    if let Some(setting) = &self.scheduler {
      setting.apply(|value| disk.set_scheduler(value))?;
    }
    if let Some(setting) = &self.readahead_kib {
      setting.apply(|&value| disk.set_readahead_kib(value))?;
    }
    if let Some(setting) = &self.apm {
      disk.set_apm(setting.value)?;
    }
    if let Some(setting) = &self.spindown {
      disk.set_spindown(setting.value)?;
    }

    Ok(())
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct GlobalDelta {
  pub alpm: Option<fs::Setting<String>>,
}

impl GlobalDelta {
  pub fn is_some(&self) -> bool {
    self.alpm.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      alpm: fs::prefer(self.alpm, &that.alpm),
    }
  }

  pub fn apply(&self) -> misstep::Result<()> {
    if let Some(setting) = &self.alpm {
      setting.apply(|value| set_alpm(value))?;
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::{
    fs as stdfs,
    os::unix::fs::symlink,
    slice::from_ref,
  };

  use super::*;
  use crate::test_support::SystemFixture;

  #[test]
  fn alpm_deduplicates_aliases_skips_unsafe_ports_and_restores_policy() {
    let fixture = SystemFixture::new();
    let host = fixture.path("/sys/devices/ata1/host0");
    let policy = host.join("link_power_management_policy");
    fixture.write(&policy, "max_performance");

    let aliases = fixture.path("/sys/class/scsi_host");
    stdfs::create_dir_all(&aliases).unwrap();
    let alias = aliases.join("host0");
    symlink(&host, &alias).unwrap();
    let duplicate = aliases.join("duplicate");
    symlink(&host, &duplicate).unwrap();

    let external_port = aliases.join("host1");
    let external_policy = external_port.join("link_power_management_policy");
    fixture.write(&external_policy, "max_performance");
    fixture.write(external_port.join("ahci_port_cmd"), "0x200000");

    let hotplug_port = aliases.join("host3");
    let hotplug_policy = hotplug_port.join("link_power_management_policy");
    fixture.write(&hotplug_policy, "max_performance");
    fixture.write(hotplug_port.join("ahci_port_cmd"), "0x40000");

    assert_eq!(alpm_paths().unwrap(), from_ref(&policy));

    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    set_alpm("min_power").unwrap();
    assert_eq!(fixture.write_count(&policy), 1);
    stdfs::remove_file(duplicate).unwrap();
    stdfs::rename(&alias, aliases.join("host2")).unwrap();

    fs::begin_settings_iteration();
    set_alpm("med_power_with_dipm").unwrap();
    assert_eq!(fixture.write_count(&policy), 2);
    assert_eq!(fs::applied_settings().len(), 1);
    assert_eq!(fixture.write_count(&external_policy), 0);
    assert_eq!(fixture.write_count(&hotplug_policy), 0);

    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(policy).unwrap(), "max_performance");
  }

  #[test]
  fn disk_aliases_restore_original_readahead() {
    let fixture = SystemFixture::new();
    let device = fixture.path("/sys/devices/block/sda");
    let readahead = device.join("queue/read_ahead_kb");
    fixture.write(&readahead, "128");
    fixture.write(device.join("removable"), "0");
    let aliases = fixture.path(BLOCK_PATH);
    stdfs::create_dir_all(&aliases).unwrap();
    symlink(&device, aliases.join("sda")).unwrap();
    symlink(&device, aliases.join("alias")).unwrap();
    let disks = Disk::all().unwrap();
    assert_eq!(disks.len(), 2);
    let _managed = fs::manage_settings();
    fs::begin_settings_iteration();
    for disk in disks {
      disk.set_readahead_kib(256).unwrap();
    }
    assert_eq!(fs::applied_settings().len(), 1);
    fs::begin_settings_iteration();
    assert!(fs::restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(readahead).unwrap(), "128");
  }
}
