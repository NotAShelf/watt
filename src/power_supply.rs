use anyhow::Context;

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

/// Represents a pattern of path suffixes used to control charge thresholds
/// for different device vendors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSupplyConfig {
    pub manufacturer: &'static str,
    pub path_start: &'static str,
    pub path_end: &'static str,
}

/// Charge threshold configs.
const POWER_SUPPLY_CONFIGS: &[PowerSupplyConfig] = &[
    PowerSupplyConfig {
        manufacturer: "Standard",
        path_start: "charge_control_start_threshold",
        path_end: "charge_control_end_threshold",
    },
    PowerSupplyConfig {
        manufacturer: "ASUS",
        path_start: "charge_control_start_percentage",
        path_end: "charge_control_end_percentage",
    },
    // Combine Huawei and ThinkPad since they use identical paths.
    PowerSupplyConfig {
        manufacturer: "ThinkPad/Huawei",
        path_start: "charge_start_threshold",
        path_end: "charge_stop_threshold",
    },
    // Framework laptop support.
    PowerSupplyConfig {
        manufacturer: "Framework",
        path_start: "charge_behaviour_start_threshold",
        path_end: "charge_behaviour_end_threshold",
    },
];

/// Represents a power supply that supports charge threshold control.
pub struct PowerSupply {
    pub name: String,
    pub path: PathBuf,
    pub config: PowerSupplyConfig,
}

impl fmt::Display for PowerSupply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "power suppply '{name}' from manufacturer '{manufacturer}'",
            name = &self.name,
            manufacturer = &self.config.manufacturer,
        )
    }
}

impl PowerSupply {
    pub fn charge_threshold_path_start(&self) -> PathBuf {
        self.path.join(self.config.path_start)
    }

    pub fn charge_threshold_path_end(&self) -> PathBuf {
        self.path.join(self.config.path_end)
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

fn is_power_supply(path: &Path) -> anyhow::Result<bool> {
    let type_path = path.join("type");

    let type_ = fs::read_to_string(&type_path)
        .with_context(|| format!("failed to read '{path}'", path = type_path.display()))?;

    Ok(type_ == "Battery")
}

/// Get all batteries in the system that support threshold control.
pub fn get_power_supplies() -> anyhow::Result<Vec<PowerSupply>> {
    const PATH: &str = "/sys/class/power_supply";

    let mut power_supplies = Vec::new();

    'entries: for entry in fs::read_dir(PATH).with_context(|| format!("failed to read '{PATH}'"))? {
        let entry = match entry {
            Ok(entry) => entry,

            Err(error) => {
                log::warn!("failed to read power supply entry: {error}");
                continue;
            }
        };

        let entry_path = entry.path();

        if !is_power_supply(&entry_path).with_context(|| {
            format!(
                "failed to determine whether if '{path}' is a power supply",
                path = entry_path.display(),
            )
        })? {
            continue;
        }

        for config in POWER_SUPPLY_CONFIGS {
            if entry_path.join(config.path_start).exists()
                && entry_path.join(config.path_end).exists()
            {
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

                    config: *config,
                });
                continue 'entries;
            }
        }
    }

    Ok(power_supplies)
}

pub fn set_charge_threshold_start(
    power_supply: &PowerSupply,
    charge_threshold_start: u8,
) -> anyhow::Result<()> {
    write(
        &power_supply.charge_threshold_path_start(),
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
        &power_supply.charge_threshold_path_end(),
        &charge_threshold_end.to_string(),
    )
    .with_context(|| format!("failed to set charge threshold end for {power_supply}"))?;

    log::info!("set battery threshold end for {power_supply} to {charge_threshold_end}%");

    Ok(())
}
