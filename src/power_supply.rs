use std::{
  fmt,
  path::{
    Path,
    PathBuf,
  },
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
#[derive(Debug, Clone, PartialEq)]
pub struct PowerSupply {
  pub name: String,
  pub path: PathBuf,

  pub type_:              String,
  pub is_from_peripheral: bool,

  pub charge_state:   Option<String>,
  pub charge_percent: Option<f64>,

  pub charge_threshold_start: f64,
  pub charge_threshold_end:   f64,

  pub drain_rate_watts: Option<f64>,

  pub threshold_config: Option<PowerSupplyThresholdConfig>,
}

impl PowerSupply {
  pub fn is_ac(&self) -> bool {
    !self.is_from_peripheral
      && matches!(
        &*self.type_,
        "Mains" | "USB_PD_DRP" | "USB_PD" | "USB_DCP" | "USB_CDP" | "USB_ACA"
      )
      || self.type_.starts_with("AC")
      || self.type_.contains("ACAD")
      || self.type_.contains("ADP")
  }
}

impl fmt::Display for PowerSupply {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "power supply '{name}'", name = self.name.yellow())?;

    if let Some(config) = self.threshold_config.as_ref() {
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

impl PowerSupply {
  pub fn from_name(name: String) -> anyhow::Result<Self> {
    let mut power_supply = Self {
      path: Path::new(POWER_SUPPLY_PATH).join(&name),
      name,
      type_: String::new(),

      charge_state: None,
      charge_percent: None,

      charge_threshold_start: 0.0,
      charge_threshold_end: 1.0,

      drain_rate_watts: None,

      is_from_peripheral: false,

      threshold_config: None,
    };

    power_supply.rescan()?;

    Ok(power_supply)
  }

  pub fn from_path(path: PathBuf) -> anyhow::Result<Self> {
    let mut power_supply = PowerSupply {
      name: path
        .file_name()
        .with_context(|| {
          format!("failed to get file name of '{path}'", path = path.display(),)
        })?
        .to_string_lossy()
        .to_string(),

      path,
      type_: String::new(),

      charge_state: None,
      charge_percent: None,

      charge_threshold_start: 0.0,
      charge_threshold_end: 1.0,

      drain_rate_watts: None,

      is_from_peripheral: false,

      threshold_config: None,
    };

    power_supply.rescan()?;

    Ok(power_supply)
  }

  pub fn all() -> anyhow::Result<Vec<PowerSupply>> {
    let mut power_supplies = Vec::new();

    for entry in fs::read_dir(POWER_SUPPLY_PATH)
      .context("failed to read power supply entries")?
      .with_context(|| {
        format!("'{POWER_SUPPLY_PATH}' doesn't exist, are you on linux?")
      })?
    {
      let entry = match entry {
        Ok(entry) => entry,

        Err(error) => {
          log::warn!("failed to read power supply entry: {error}");
          continue;
        },
      };

      power_supplies.push(PowerSupply::from_path(entry.path())?);
    }

    Ok(power_supplies)
  }

  pub fn rescan(&mut self) -> anyhow::Result<()> {
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
      if let Some(energy_full) =
        fs::read_n::<u64>(self.path.join("energy_full")).with_context(|| {
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

      self.charge_percent = fs::read_n::<u64>(self.path.join("capacity"))
        .with_context(|| format!("failed to read {self} charge percent"))?
        .map(|percent| percent as f64 / 100.0);

      self.charge_threshold_start =
        fs::read_n::<u64>(self.path.join("charge_control_start_threshold"))
          .with_context(|| {
            format!("failed to read {self} charge threshold start")
          })?
          .map_or(0.0, |percent| percent as f64 / 100.0);

      self.charge_threshold_end =
        fs::read_n::<u64>(self.path.join("charge_control_end_threshold"))
          .with_context(|| {
            format!("failed to read {self} charge threshold end")
          })?
          .map_or(100.0, |percent| percent as f64 / 100.0);

      self.drain_rate_watts =
        match fs::read_n::<i64>(self.path.join("power_now"))
          .with_context(|| format!("failed to read {self} power drain"))?
        {
          Some(drain) => Some(drain as f64),

          None => {
            let current_ua =
              fs::read_n::<i32>(self.path.join("current_now"))
                .with_context(|| format!("failed to read {self} current"))?;

            let voltage_uv =
              fs::read_n::<i32>(self.path.join("voltage_now"))
                .with_context(|| format!("failed to read {self} voltage"))?;

            current_ua.zip(voltage_uv).map(|(current, voltage)| {
              // Power (W) = Voltage (V) * Current (A)
              // (v / 1e6 V) * (c / 1e6 A) = (v * c / 1e12) W
              current as f64 * voltage as f64 / 1e12
            })
          },
        };

      self.threshold_config = POWER_SUPPLY_THRESHOLD_CONFIGS
        .iter()
        .find(|config| {
          self.path.join(config.path_start).exists()
            && self.path.join(config.path_end).exists()
        })
        .copied();
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
    fs::write(
      &self.charge_threshold_path_start().ok_or_else(|| {
        anyhow!(
          "power supply '{name}' does not support changing charge threshold \
           levels",
          name = self.name,
        )
      })?,
      &((charge_threshold_start * 100.0) as u8).to_string(),
    )
    .with_context(|| {
      format!("failed to set charge threshold start for {self}")
    })?;

    self.charge_threshold_start = charge_threshold_start;

    log::info!(
      "set battery threshold start for {self} to {charge_threshold_start}%"
    );

    Ok(())
  }

  pub fn set_charge_threshold_end(
    &mut self,
    charge_threshold_end: f64,
  ) -> anyhow::Result<()> {
    fs::write(
      &self.charge_threshold_path_end().ok_or_else(|| {
        anyhow!(
          "power supply '{name}' does not support changing charge threshold \
           levels",
          name = self.name,
        )
      })?,
      &((charge_threshold_end * 100.0) as u8).to_string(),
    )
    .with_context(|| {
      format!("failed to set charge threshold end for {self}")
    })?;

    self.charge_threshold_end = charge_threshold_end;

    log::info!(
      "set battery threshold end for {self} to {charge_threshold_end}%"
    );

    Ok(())
  }

  pub fn get_available_platform_profiles() -> anyhow::Result<Vec<String>> {
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
    fs::read("/sys/firmware/acpi/platform_profile")
      .context("failed to read platform profile")?
      .context("failed to find platform profile")
  }
}
