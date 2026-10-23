use std::{
  error,
  fmt,
  hash,
  path::{
    Path,
    PathBuf,
  },
  str,
};

use anyhow::{
  Context,
  anyhow,
  bail,
};
use yansi::Paint as _;

use crate::fs;

/// Represents a pattern of path suffixes used to control charge thresholds
/// for different device vendors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSupplyThresholdConfig {
  pub manufacturer: &'static str,
  pub path_start:   &'static str,
  pub path_end:     &'static str,
}

/// Power supply threshold configs.
const POWER_SUPPLY_THRESHOLD_CONFIGS: &[PowerSupplyThresholdConfig] = &[
  PowerSupplyThresholdConfig {
    manufacturer: "Standard",
    path_start:   "charge_control_start_threshold",
    path_end:     "charge_control_end_threshold",
  },
  PowerSupplyThresholdConfig {
    manufacturer: "ASUS",
    path_start:   "charge_control_start_percentage",
    path_end:     "charge_control_end_percentage",
  },
  // Combine Huawei and ThinkPad since they use identical paths.
  PowerSupplyThresholdConfig {
    manufacturer: "ThinkPad/Huawei",
    path_start:   "charge_start_threshold",
    path_end:     "charge_stop_threshold",
  },
  // Framework laptop support.
  PowerSupplyThresholdConfig {
    manufacturer: "Framework",
    path_start:   "charge_behaviour_start_threshold",
    path_end:     "charge_behaviour_end_threshold",
  },
];

/// Represents a power supply that supports charge threshold control.
#[derive(Debug, Clone)]
pub struct PowerSupply {
  pub name: String,
  pub path: PathBuf,

  pub type_:              String,
  pub is_from_peripheral: bool,

  pub charge_state:   Option<String>,
  pub charge_percent: Option<f64>,

  pub cycles: Option<u64>,
  pub health: Option<f64>,

  pub charge_threshold_start: f64,
  pub charge_threshold_end:   f64,

  pub drain_rate_watts: Option<f64>,

  pub threshold_config: Option<PowerSupplyThresholdConfig>,
}

impl PartialEq for PowerSupply {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name
  }
}

impl Eq for PowerSupply {}

impl hash::Hash for PowerSupply {
  fn hash<H: hash::Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl PowerSupply {
  pub fn is_ac(&self) -> bool {
    !self.is_from_peripheral
      && (matches!(
        &*self.type_,
        "Mains"
          | "Wireless"
          | "USB_PD_DRP"
          | "USB_PD"
          | "USB_DCP"
          | "USB_CDP"
          | "USB_ACA"
      ) || self.type_.starts_with("AC")
        || self.type_.contains("ACAD")
        || self.type_.contains("ADP"))
  }
}

impl fmt::Display for PowerSupply {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "power supply '{name}'", name = self.name.yellow())?;

    if let Some(config) = &self.threshold_config {
      write!(
        f,
        " from manufacturer '{manufacturer}'",
        manufacturer = config.manufacturer.green(),
      )?;
    }

    Ok(())
  }
}

const POWER_SUPPLY_PATH: &str = "/sys/class/power_supply";

fn threshold_percentage(value: f64) -> anyhow::Result<u8> {
  let percent = (value * 100.0).round();
  // Account for binary rounding when percentages are normalized to 0..=1.
  if !(0.0..=1.0).contains(&value)
    || (value - percent / 100.0).abs() > f64::EPSILON
  {
    bail!(
      "charge threshold must represent an integer percentage from 0 to 100"
    );
  }
  Ok(percent as u8)
}

/// Discovered power supplies and errors from incomplete discovery.
#[derive(Debug)]
pub struct PowerSupplyInventory {
  pub power_supplies: Vec<PowerSupply>,
  pub failures:       Vec<String>,
}

impl PowerSupplyInventory {
  /// Reads power-supply capabilities and telemetry without changing settings.
  pub fn scan() -> anyhow::Result<Self> {
    Self::scan_in(Path::new(POWER_SUPPLY_PATH))
  }

  fn scan_in(root: &Path) -> anyhow::Result<Self> {
    log::trace!("detecting power supplies...");

    let mut power_supplies = Vec::new();
    let mut failures = Vec::new();

    log::debug!("scanning power supplies in {}", root.display());

    for entry in fs::read_dir(root)
      .context("failed to read power supply entries")?
      .with_context(|| {
        format!("'{}' doesn't exist, are you on linux?", root.display())
      })?
    {
      let entry = match entry {
        Ok(entry) => entry,

        Err(error) => {
          failures.push(format!("failed to read power supply entry: {error}"));
          continue;
        },
      };
      let Some(path) = fs::canonicalize(entry.path())? else {
        continue;
      };

      let mut power_supply = PowerSupply {
        name: entry.file_name().to_string_lossy().into_owned(),

        path,
        type_: String::new(),

        charge_state: None,
        charge_percent: None,

        cycles: None,
        health: None,

        charge_threshold_start: 0.0,
        charge_threshold_end: 1.0,

        drain_rate_watts: None,

        is_from_peripheral: false,

        threshold_config: None,
      };

      if let Err(error) = power_supply.scan() {
        failures.push(format!("failed to scan {power_supply}: {error:#}"));
        continue;
      }

      power_supplies.push(power_supply);
    }

    log::trace!("detected {len} power supplies", len = power_supplies.len());

    Ok(Self {
      power_supplies,
      failures,
    })
  }
}

impl PowerSupply {
  pub fn all() -> anyhow::Result<Vec<PowerSupply>> {
    let inventory = PowerSupplyInventory::scan()?;
    for failure in &inventory.failures {
      log::warn!("{failure}");
    }
    Ok(inventory.power_supplies)
  }

  /// Reads optional numeric telemetry.
  ///
  /// Power-supply drivers may omit properties, and buggy firmware can expose
  /// temporarily malformed values. Neither case should prevent Watt from
  /// using the remaining properties of the device.
  fn read_telemetry_n<N>(&self, property: &str) -> anyhow::Result<Option<N>>
  where
    N: str::FromStr,
    N::Err: error::Error + Send + Sync + 'static,
  {
    let path = self.path.join(property);
    let Some(value) = fs::read(&path)? else {
      return Ok(None);
    };

    match value.parse() {
      Ok(value) => Ok(Some(value)),
      Err(error) => {
        log::warn!(
          "ignoring invalid numeric property '{property}' from {self}: {error}"
        );
        Ok(None)
      },
    }
  }

  fn capacity_ratio(
    &self,
    full_property: &str,
    design_property: &str,
  ) -> anyhow::Result<Option<f64>> {
    let full = self.read_telemetry_n::<u64>(full_property)?;
    let design = self.read_telemetry_n::<u64>(design_property)?;

    Ok(
      full
        .zip(design)
        .filter(|(_, design)| *design != 0)
        .map(|(full, design)| full as f64 / design as f64),
    )
  }

  fn scan_health(&self) -> anyhow::Result<Option<f64>> {
    if let Some(health) = self.read_telemetry_n::<u64>("state_of_health")? {
      return Ok(Some(health as f64 / 100.0));
    }

    if let Some(health) =
      self.capacity_ratio("energy_full", "energy_full_design")?
    {
      return Ok(Some(health));
    }

    self.capacity_ratio("charge_full", "charge_full_design")
  }

  fn scan(&mut self) -> anyhow::Result<()> {
    log::trace!("scanning power supply '{name}'", name = self.name);

    if !self.path.exists() {
      bail!("{self} does not exist");
    }

    self.type_ = {
      let type_path = self.path.join("type");

      fs::read(&type_path)
        .with_context(|| {
          format!("failed to read '{path}'", path = type_path.display())
        })?
        .with_context(|| {
          format!("'{path}' doesn't exist", path = type_path.display())
        })?
    };

    self.is_from_peripheral = 'is_from_peripheral: {
      let name_lower = self.name.to_lowercase();

      log::trace!(
        "power supply '{name}' type: {type_}",
        name = self.name,
        type_ = self.type_
      );

      // Common peripheral battery names.
      if name_lower.contains("mouse")
        || name_lower.contains("keyboard")
        || name_lower.contains("trackpad")
        || name_lower.contains("gamepad")
        || name_lower.contains("controller")
        || name_lower.contains("headset")
        || name_lower.contains("headphone")
      {
        break 'is_from_peripheral true;
      }

      // Small capacity batteries are likely not laptop batteries.
      if let Some(energy_full) = self
        .read_telemetry_n::<u64>("energy_full")
        .with_context(|| {
          format!("failed to read the max charge {self} can hold")
        })?
      {
        // Most laptop batteries are at least 20,000,000 µWh (20 Wh).
        // Peripheral batteries are typically much smaller.
        if energy_full < 10_000_000 {
          // 10 Wh in µWh.
          break 'is_from_peripheral true;
        }
      }
      // Check for model name that indicates a peripheral
      if let Some(model_name) = fs::read(self.path.join("model_name"))
        .with_context(|| format!("failed to read the model name of {self}"))?
      {
        let model_name_lower = model_name.to_lowercase();
        if model_name_lower.contains("bluetooth")
          || model_name_lower.contains("wireless")
        {
          break 'is_from_peripheral true;
        }
      }

      false
    };

    if self.type_ == "Battery" {
      self.charge_state = fs::read(self.path.join("status"))
        .with_context(|| format!("failed to read {self} charge status"))?;

      self.charge_percent = self
        .read_telemetry_n::<u64>("capacity")
        .with_context(|| format!("failed to read {self} charge percent"))?
        .map(|percent| percent as f64 / 100.0);

      self.cycles = self
        .read_telemetry_n::<u64>("cycle_count")
        .with_context(|| format!("failed to read {self} cycle count"))?;

      // `health` is a textual condition such as "Good", not a percentage.
      // Prefer the kernel's numeric state-of-health property, then derive the
      // value from a matching energy or charge capacity pair.
      self.health = self
        .scan_health()
        .with_context(|| format!("failed to read {self} health"))?;

      self.threshold_config = POWER_SUPPLY_THRESHOLD_CONFIGS
        .iter()
        .find(|config| {
          self.path.join(config.path_start).exists()
            && self.path.join(config.path_end).exists()
        })
        .copied();

      self.charge_threshold_start = if let Some(config) = self.threshold_config
      {
        fs::read_n::<u64>(self.path.join(config.path_start))
          .with_context(|| {
            format!("failed to read {self} charge threshold start")
          })?
          .map_or(0.0, |percent| percent as f64 / 100.0)
      } else {
        0.0
      };

      self.charge_threshold_end = if let Some(config) = self.threshold_config {
        fs::read_n::<u64>(self.path.join(config.path_end))
          .with_context(|| {
            format!("failed to read {self} charge threshold end")
          })?
          .map_or(1.0, |percent| percent as f64 / 100.0)
      } else {
        1.0
      };

      self.drain_rate_watts = match self
        .read_telemetry_n::<i64>("power_now")
        .with_context(|| format!("failed to read {self} power drain"))?
      {
        Some(drain) => Some(drain as f64 / 1e6),

        None => {
          let current_ua = self
            .read_telemetry_n::<i32>("current_now")
            .with_context(|| format!("failed to read {self} current"))?;

          let voltage_uv = self
            .read_telemetry_n::<i32>("voltage_now")
            .with_context(|| format!("failed to read {self} voltage"))?;

          current_ua.zip(voltage_uv).map(|(current, voltage)| {
            // Power (W) = Voltage (V) * Current (A)
            // (v / 1e6 V) * (c / 1e6 A) = (v * c / 1e12) W
            current as f64 * voltage as f64 / 1e12
          })
        },
      };

      log::debug!(
        "power supply '{name}' threshold config: {threshold_config:?}",
        name = self.name,
        threshold_config = self.threshold_config,
      );
    }

    Ok(())
  }

  pub fn charge_threshold_path_start(&self) -> Option<PathBuf> {
    self
      .threshold_config
      .map(|config| self.path.join(config.path_start))
  }

  pub fn charge_threshold_path_end(&self) -> Option<PathBuf> {
    self
      .threshold_config
      .map(|config| self.path.join(config.path_end))
  }

  pub fn set_charge_threshold_start(
    &mut self,
    charge_threshold_start: f64,
  ) -> anyhow::Result<()> {
    let percent = threshold_percentage(charge_threshold_start)?;
    fs::write(
      &self.charge_threshold_path_start().ok_or_else(|| {
        anyhow!(
          "power supply '{name}' does not support changing charge threshold \
           levels",
          name = self.name,
        )
      })?,
      &percent.to_string(),
    )
    .with_context(|| {
      format!("failed to set charge threshold start for {self}")
    })?;

    self.charge_threshold_start = f64::from(percent) / 100.0;

    log::trace!("set battery threshold start for {self} to {percent}%");

    Ok(())
  }

  pub fn set_charge_threshold_end(
    &mut self,
    charge_threshold_end: f64,
  ) -> anyhow::Result<()> {
    let percent = threshold_percentage(charge_threshold_end)?;
    fs::write(
      &self.charge_threshold_path_end().ok_or_else(|| {
        anyhow!(
          "power supply '{name}' does not support changing charge threshold \
           levels",
          name = self.name,
        )
      })?,
      &percent.to_string(),
    )
    .with_context(|| {
      format!("failed to set charge threshold end for {self}")
    })?;

    self.charge_threshold_end = f64::from(percent) / 100.0;

    log::trace!("set battery threshold end for {self} to {percent}%");

    Ok(())
  }

  pub fn get_available_platform_profiles() -> anyhow::Result<Vec<String>> {
    log::trace!("reading available platform profiles");

    let path = "/sys/firmware/acpi/platform_profile_choices";

    let Some(content) = fs::read(path)
      .context("failed to read available ACPI platform profiles")?
    else {
      return Ok(Vec::new());
    };

    Ok(
      content
        .split_whitespace()
        .map(ToString::to_string)
        .collect(),
    )
  }

  /// Sets the platform profile.
  /// This changes the system performance, temperature, fan, and other hardware
  /// related characteristics.
  ///
  /// Also see [`The Kernel docs`] for this.
  ///
  /// [`The Kernel docs`]: <https://docs.kernel.org/userspace-api/sysfs-platform_profile.html>
  pub fn set_platform_profile(profile: &str) -> anyhow::Result<()> {
    log::trace!("setting platform profile to '{profile}'");

    let profiles = Self::get_available_platform_profiles()?;

    if !profiles
      .iter()
      .any(|avail_profile| avail_profile == profile)
    {
      bail!(
        "profile '{profile}' is not available for system. valid profiles: \
         {profiles}",
        profiles = profiles.join(", "),
      );
    }

    fs::write("/sys/firmware/acpi/platform_profile", profile).context(
      "this probably means that your system does not support changing ACPI \
       profiles",
    )
  }

  pub fn platform_profile() -> anyhow::Result<String> {
    log::trace!("reading current platform profile");

    fs::read("/sys/firmware/acpi/platform_profile")
      .context("failed to read platform profile")?
      .context("failed to find platform profile")
  }
}

#[derive(Default, Debug, Clone, PartialEq)]
#[must_use]
pub struct Delta {
  pub charge_threshold_start: Option<f64>,
  pub charge_threshold_end:   Option<f64>,
}

impl Delta {
  pub fn is_some(&self) -> bool {
    self.charge_threshold_start.is_some() && self.charge_threshold_end.is_some()
  }

  pub fn or(self, that: &Self) -> Self {
    Self {
      charge_threshold_start: self
        .charge_threshold_start
        .or(that.charge_threshold_start),
      charge_threshold_end:   self
        .charge_threshold_end
        .or(that.charge_threshold_end),
    }
  }

  pub fn apply(&self, power_supply: &mut PowerSupply) -> anyhow::Result<()> {
    for value in [self.charge_threshold_start, self.charge_threshold_end]
      .into_iter()
      .flatten()
    {
      threshold_percentage(value)?;
    }
    if let Some(charge_threshold_start) = self.charge_threshold_start {
      power_supply.set_charge_threshold_start(charge_threshold_start)?;
    }

    if let Some(charge_threshold_end) = self.charge_threshold_end {
      power_supply.set_charge_threshold_end(charge_threshold_end)?;
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::{
    fs,
    path::PathBuf,
  };

  use super::{
    Delta,
    PowerSupply,
    PowerSupplyInventory,
  };
  use crate::test_support::SystemFixture;

  #[test]
  fn threshold_writes_preserve_integer_percentages_and_reject_invalid_pairs() {
    let fixture = SystemFixture::new();
    let battery = "/sys/class/power_supply/BAT0";
    fixture.write(format!("{battery}/type"), "Battery");
    let start = format!("{battery}/charge_control_start_threshold");
    let end = format!("{battery}/charge_control_end_threshold");
    fixture.write(&start, "40");
    fixture.write(&end, "80");
    let mut supply = PowerSupply::all().unwrap().pop().unwrap();
    for percent in 0..=100 {
      let value = f64::from(percent) / 100.0;
      supply.set_charge_threshold_start(value).unwrap();
      supply.set_charge_threshold_end(value).unwrap();
      assert_eq!(
        fs::read_to_string(fixture.path(&start)).unwrap(),
        percent.to_string()
      );
      assert_eq!(
        fs::read_to_string(fixture.path(&end)).unwrap(),
        percent.to_string()
      );
    }
    let writes = fixture.write_count(&start) + fixture.write_count(&end);
    for invalid in [-0.01, 1.01, 0.405, f64::NAN, f64::INFINITY] {
      assert!(supply.set_charge_threshold_start(invalid).is_err());
      assert!(supply.set_charge_threshold_end(invalid).is_err());
      assert!(
        Delta {
          charge_threshold_start: Some(0.4),
          charge_threshold_end:   Some(invalid),
        }
        .apply(&mut supply)
        .is_err()
      );
    }
    assert_eq!(
      fixture.write_count(&start) + fixture.write_count(&end),
      writes
    );
    assert_eq!(supply.charge_threshold_start, 1.0);
    assert_eq!(supply.charge_threshold_end, 1.0);
  }

  struct BatteryFixture {
    system: SystemFixture,
    path:   PathBuf,
  }

  impl BatteryFixture {
    fn new() -> Self {
      let system = SystemFixture::new();
      let fixture = Self {
        path: system.path("/sys/class/power_supply/BAT0"),
        system,
      };
      fixture.write("type", "Battery");
      fixture
    }

    fn write(&self, name: &str, value: &str) {
      self.system.write(self.path.join(name), value);
    }

    fn power_supply(&self) -> PowerSupply {
      PowerSupply {
        name:                   "BAT0".to_owned(),
        path:                   self.path.clone(),
        type_:                  String::new(),
        is_from_peripheral:     false,
        charge_state:           None,
        charge_percent:         None,
        cycles:                 None,
        health:                 None,
        charge_threshold_start: 0.0,
        charge_threshold_end:   1.0,
        drain_rate_watts:       None,
        threshold_config:       None,
      }
    }
  }

  #[test]
  fn scan_health_good_falls_back_to_energy_ratio_when_state_of_health_is_missing()
   {
    let fixture = BatteryFixture::new();
    fixture.write("health", "Good");
    fixture.write("energy_full", "75000000");
    fixture.write("energy_full_design", "100000000");

    let mut power_supply = fixture.power_supply();
    power_supply.scan().expect("scan battery fixture");

    assert_eq!(power_supply.health, Some(0.75));
  }

  #[test]
  fn scan_state_of_health_percentage_overrides_energy_ratio_with_health_good() {
    let fixture = BatteryFixture::new();
    fixture.write("health", "Good");
    fixture.write("state_of_health", "82");
    fixture.write("energy_full", "40000000");
    fixture.write("energy_full_design", "100000000");

    let mut power_supply = fixture.power_supply();
    power_supply.scan().expect("scan battery fixture");

    assert_eq!(power_supply.health, Some(0.82));
  }

  #[test]
  fn scan_health_falls_back_to_charge_ratio() {
    let fixture = BatteryFixture::new();
    fixture.write("health", "Good");
    fixture.write("charge_full", "4100000");
    fixture.write("charge_full_design", "5000000");

    let mut power_supply = fixture.power_supply();
    power_supply.scan().expect("scan battery fixture");

    assert_eq!(power_supply.health, Some(0.82));
  }

  #[test]
  fn scan_health_ignores_malformed_optional_source_and_uses_next_pair() {
    let fixture = BatteryFixture::new();
    fixture.write("state_of_health", "Good");
    fixture.write("energy_full", "invalid");
    fixture.write("energy_full_design", "100000000");
    fixture.write("charge_full", "4500000");
    fixture.write("charge_full_design", "5000000");

    let mut power_supply = fixture.power_supply();
    power_supply.scan().expect("scan battery fixture");

    assert_eq!(power_supply.health, Some(0.9));
  }

  #[test]
  fn discovery_keeps_valid_supplies_when_another_supply_is_broken() {
    let fixture = BatteryFixture::new();
    fs::remove_file(fixture.path.join("type")).expect("remove fixture type");

    let broken = fixture.path.join("broken");
    fs::create_dir(&broken).expect("create broken supply");

    let battery = fixture.path.join("BAT0");
    fs::create_dir(&battery).expect("create battery supply");
    fs::write(battery.join("type"), "Battery").expect("write battery type");
    fs::write(battery.join("energy_full"), "80000000")
      .expect("write full energy");
    fs::write(battery.join("energy_full_design"), "100000000")
      .expect("write design energy");

    let inventory = PowerSupplyInventory::scan_in(&fixture.path)
      .expect("discover power supplies");
    assert!(
      inventory
        .failures
        .iter()
        .any(|failure| failure.contains("broken"))
    );
    let power_supplies = inventory.power_supplies;

    assert_eq!(power_supplies.len(), 1);
    assert_eq!(power_supplies[0].name, "BAT0");
    assert_eq!(power_supplies[0].health, Some(0.8));
  }
}
