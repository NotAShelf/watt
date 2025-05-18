use anyhow::Context;

use std::{
    fmt, fs,
    os::macos::fs::MetadataExt,
    path::{Path, PathBuf},
};

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

impl PowerSupply {
    pub fn charge_threshold_path_start(&self) -> Option<PathBuf> {
        self.threshold_config
            .map(|config| self.path.join(config.path_start))
    }

    pub fn charge_threshold_path_end(&self) -> Option<PathBuf> {
        self.threshold_config
            .map(|config| self.path.join(config.path_end))
    }
}

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

fn is_battery(path: &Path) -> anyhow::Result<bool> {
    let type_path = path.join("type");

    let type_ = fs::read_to_string(&type_path)
        .with_context(|| format!("failed to read '{path}'", path = type_path.display()))?;

    Ok(type_ == "Battery")
}

const POWER_SUPPLY_PATH: &str = "/sys/class/power_supply";

/// Get power supply.
pub fn get_power_supply(name: &str) -> anyhow::Result<PowerSupply> {
    let entry_path = Path::new(POWER_SUPPLY_PATH).join(name);

    let threshold_config = is_battery(&entry_path)
        .with_context(|| {
            format!(
                "failed to determine what type of power supply '{path}' is",
                path = entry_path.display(),
            )
        })?
        .then(|| {
            for config in POWER_SUPPLY_THRESHOLD_CONFIGS {
                if entry_path.join(config.path_start).exists()
                    && entry_path.join(config.path_end).exists()
                {
                    return Some(*config);
                }
            }

            None
        })
        .flatten();

    Ok(PowerSupply {
        name: name.to_owned(),
        path: entry_path,
        threshold_config,
    })
}

/// Get all power supplies.
pub fn get_power_supplies() -> anyhow::Result<Vec<PowerSupply>> {
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

        let entry_path = entry.path();

        let mut power_supply_config = None;

        if is_battery(&entry_path).with_context(|| {
            format!(
                "failed to determine what type of power supply '{path}' is",
                path = entry_path.display(),
            )
        })? {
            for config in POWER_SUPPLY_THRESHOLD_CONFIGS {
                if entry_path.join(config.path_start).exists()
                    && entry_path.join(config.path_end).exists()
                {
                    power_supply_config = Some(*config);
                    break;
                }
            }
        }

        power_supplies.push(PowerSupply {
            name: entry_path
                .file_name()
                .with_context(|| {
                    format!(
                        "failed to get file name of '{path}'",
                        path = entry_path.display(),
                    )
                })?
                .to_string_lossy()
                .to_string(),

            path: entry_path,

            threshold_config: power_supply_config,
        });
    }

    Ok(power_supplies)
}

pub fn set_charge_threshold_start(
    power_supply: &PowerSupply,
    charge_threshold_start: u8,
) -> anyhow::Result<()> {
    write(
        &power_supply.charge_threshold_path_start().ok_or_else(|| {
            anyhow::anyhow!(
                "power supply '{name}' does not support changing charge threshold levels",
                name = power_supply.name,
            )
        })?,
        &charge_threshold_start.to_string(),
    )
    .with_context(|| format!("failed to set charge threshold start for {power_supply}"))?;

    log::info!("set battery threshold start for {power_supply} to {charge_threshold_start}%");

    Ok(())
}

pub fn set_charge_threshold_end(
    power_supply: &PowerSupply,
    charge_threshold_end: u8,
) -> anyhow::Result<()> {
    write(
        &power_supply.charge_threshold_path_end().ok_or_else(|| {
            anyhow::anyhow!(
                "power supply '{name}' does not support changing charge threshold levels",
                name = power_supply.name,
            )
        })?,
        &charge_threshold_end.to_string(),
    )
    .with_context(|| format!("failed to set charge threshold end for {power_supply}"))?;

    log::info!("set battery threshold end for {power_supply} to {charge_threshold_end}%");

    Ok(())
}
