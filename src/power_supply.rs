use anyhow::{Context, bail};

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

// TODO: Migrate to central utils file. Same exists in cpu.rs.
fn write(path: impl AsRef<Path>, value: &str) -> anyhow::Result<()> {
    let path = path.as_ref();

    fs::write(path, value).with_context(|| {
        format!(
            "failed to write '{value}' to '{path}'",
            path = path.display(),
        )
    })
}

/// Represents a pattern of path suffixes used to control charge thresholds
/// for different device vendors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSupplyThresholdConfig {
    pub manufacturer: &'static str,
    pub path_start: &'static str,
    pub path_end: &'static str,
}

/// Power supply threshold configs.
const POWER_SUPPLY_THRESHOLD_CONFIGS: &[PowerSupplyThresholdConfig] = &[
    PowerSupplyThresholdConfig {
        manufacturer: "Standard",
        path_start: "charge_control_start_threshold",
        path_end: "charge_control_end_threshold",
    },
    PowerSupplyThresholdConfig {
        manufacturer: "ASUS",
        path_start: "charge_control_start_percentage",
        path_end: "charge_control_end_percentage",
    },
    // Combine Huawei and ThinkPad since they use identical paths.
    PowerSupplyThresholdConfig {
        manufacturer: "ThinkPad/Huawei",
        path_start: "charge_start_threshold",
        path_end: "charge_stop_threshold",
    },
    // Framework laptop support.
    PowerSupplyThresholdConfig {
        manufacturer: "Framework",
        path_start: "charge_behaviour_start_threshold",
        path_end: "charge_behaviour_end_threshold",
    },
];

/// Represents a power supply that supports charge threshold control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSupply {
    pub name: String,
    pub path: PathBuf,
    pub threshold_config: Option<PowerSupplyThresholdConfig>,
}

impl fmt::Display for PowerSupply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "power supply '{name}'", name = &self.name)?;

        if let Some(config) = self.threshold_config.as_ref() {
            write!(
                f,
                " from manufacturer '{manufacturer}'",
                manufacturer = config.manufacturer,
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

            threshold_config: None,
        };

        power_supply.rescan()?;

        Ok(power_supply)
    }

    pub fn all() -> anyhow::Result<Vec<PowerSupply>> {
        let mut power_supplies = Vec::new();

        for entry in fs::read_dir(POWER_SUPPLY_PATH)
            .with_context(|| format!("failed to read '{POWER_SUPPLY_PATH}'"))?
        {
            let entry = match entry {
                Ok(entry) => entry,

                Err(error) => {
                    log::warn!("failed to read power supply entry: {error}");
                    continue;
                }
            };

            power_supplies.push(PowerSupply::from_path(entry.path())?);
        }

        Ok(power_supplies)
    }

    fn get_type(&self) -> anyhow::Result<String> {
        let type_path = self.path.join("type");

        let type_ = fs::read_to_string(&type_path)
            .with_context(|| format!("failed to read '{path}'", path = type_path.display()))?;

        Ok(type_)
    }

    pub fn rescan(&mut self) -> anyhow::Result<()> {
        if !self.path.exists() {
            bail!("{self} does not exist");
        }

        let threshold_config = self
            .get_type()
            .with_context(|| format!("failed to determine what type of power supply '{self}' is"))?
            .eq("Battery")
            .then(|| {
                for config in POWER_SUPPLY_THRESHOLD_CONFIGS {
                    if self.path.join(config.path_start).exists()
                        && self.path.join(config.path_end).exists()
                    {
                        return Some(*config);
                    }
                }

                None
            })
            .flatten();

        self.threshold_config = threshold_config;

        Ok(())
    }

    pub fn charge_threshold_path_start(&self) -> Option<PathBuf> {
        self.threshold_config
            .map(|config| self.path.join(config.path_start))
    }

    pub fn charge_threshold_path_end(&self) -> Option<PathBuf> {
        self.threshold_config
            .map(|config| self.path.join(config.path_end))
    }

    pub fn set_charge_threshold_start(&self, charge_threshold_start: u8) -> anyhow::Result<()> {
        write(
            &self.charge_threshold_path_start().ok_or_else(|| {
                anyhow::anyhow!(
                    "power supply '{name}' does not support changing charge threshold levels",
                    name = self.name,
                )
            })?,
            &charge_threshold_start.to_string(),
        )
        .with_context(|| format!("failed to set charge threshold start for {self}"))?;

        log::info!("set battery threshold start for {self} to {charge_threshold_start}%");

        Ok(())
    }

    pub fn set_charge_threshold_end(&self, charge_threshold_end: u8) -> anyhow::Result<()> {
        write(
            &self.charge_threshold_path_end().ok_or_else(|| {
                anyhow::anyhow!(
                    "power supply '{name}' does not support changing charge threshold levels",
                    name = self.name,
                )
            })?,
            &charge_threshold_end.to_string(),
        )
        .with_context(|| format!("failed to set charge threshold end for {self}"))?;

        log::info!("set battery threshold end for {self} to {charge_threshold_end}%");

        Ok(())
    }

    pub fn get_available_platform_profiles() -> Vec<String> {
        let path = "/sys/firmware/acpi/platform_profile_choices";

        let Ok(content) = fs::read_to_string(path) else {
            return Vec::new();
        };

        content
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    }

    /// Sets the platform profile.
    /// This changes the system performance, temperature, fan, and other hardware replated characteristics.
    ///
    /// Also see [`The Kernel docs`] for this.
    ///
    /// [`The Kernel docs`]: <https://docs.kernel.org/userspace-api/sysfs-platform_profile.html>
    pub fn set_platform_profile(profile: &str) -> anyhow::Result<()> {
        let profiles = Self::get_available_platform_profiles();

        if !profiles
            .iter()
            .any(|avail_profile| avail_profile == profile)
        {
            bail!(
                "profile '{profile}' is not available for system. valid profiles: {profiles}",
                profiles = profiles.join(", "),
            );
        }

        write("/sys/firmware/acpi/platform_profile", profile)
            .context("this probably means that your system does not support changing ACPI profiles")
    }
}
