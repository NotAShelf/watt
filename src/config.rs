use std::{fs, path::Path};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[derive(Serialize, Deserialize, clap::Parser, Default, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct CpuDelta {
    /// The CPUs to apply the changes to. When unspecified, will be applied to all CPUs.
    #[arg(short = 'c', long = "for")]
    #[serde(rename = "for", skip_serializing_if = "is_default")]
    pub for_: Option<Vec<u32>>,

    /// Set the CPU governor.
    #[arg(short = 'g', long)]
    #[serde(skip_serializing_if = "is_default")]
    pub governor: Option<String>, // TODO: Validate with clap for available governors.

    /// Set CPU Energy Performance Preference (EPP). Short form: --epp.
    #[arg(short = 'p', long, alias = "epp")]
    #[serde(skip_serializing_if = "is_default")]
    pub energy_performance_preference: Option<String>, // TODO: Validate with clap for available governors.

    /// Set CPU Energy Performance Bias (EPB). Short form: --epb.
    #[arg(short = 'b', long, alias = "epb")]
    #[serde(skip_serializing_if = "is_default")]
    pub energy_performance_bias: Option<String>, // TODO: Validate with clap for available governors.

    /// Set minimum CPU frequency in MHz. Short form: --freq-min.
    #[arg(short = 'f', long, alias = "freq-min", value_parser = clap::value_parser!(u64).range(1..=10_000))]
    #[serde(skip_serializing_if = "is_default")]
    pub frequency_mhz_minimum: Option<u64>,

    /// Set maximum CPU frequency in MHz. Short form: --freq-max.
    #[arg(short = 'F', long, alias = "freq-max", value_parser = clap::value_parser!(u64).range(1..=10_000))]
    #[serde(skip_serializing_if = "is_default")]
    pub frequency_mhz_maximum: Option<u64>,

    /// Set turbo boost behaviour. Has to be for all CPUs.
    #[arg(short = 't', long, conflicts_with = "for_")]
    #[serde(skip_serializing_if = "is_default")]
    pub turbo: Option<bool>,
}

#[derive(Serialize, Deserialize, clap::Parser, Default, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct PowerDelta {
    /// The power supplies to apply the changes to. When unspecified, will be applied to all power supplies.
    #[arg(short = 'p', long = "for")]
    #[serde(rename = "for", skip_serializing_if = "is_default")]
    pub for_: Option<Vec<String>>,

    /// Set the percentage that the power supply has to drop under for charging to start. Short form: --charge-start.
    #[arg(short = 'c', long, alias = "charge-start", value_parser = clap::value_parser!(u8).range(0..=100))]
    #[serde(skip_serializing_if = "is_default")]
    pub charge_threshold_start: Option<u8>,

    /// Set the percentage where charging will stop. Short form: --charge-end.
    #[arg(short = 'C', long, alias = "charge-end", value_parser = clap::value_parser!(u8).range(0..=100))]
    #[serde(skip_serializing_if = "is_default")]
    pub charge_threshold_end: Option<u8>,

    /// Set ACPI platform profile. Has to be for all power supplies.
    #[arg(short = 'f', long, alias = "profile", conflicts_with = "for_")]
    #[serde(skip_serializing_if = "is_default")]
    pub platform_profile: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged, rename_all = "kebab-case")]
pub enum Condition {
    ChargeLessThan(u8),
    ChargeMoreThan(u8),

    TemperatureLessThan(u8),
    TemperatureMoreThan(u8),

    UtilizationLessThan(u8),
    UtilizationMoreThan(u8),

    Charging,
    OnBattery,

    False,
    #[default]
    True,

    All(Vec<Condition>),
    Any(Vec<Condition>),

    Not(Box<Condition>),
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DaemonConfigLayer {
    priority: u8,

    #[serde(default, skip_serializing_if = "is_default")]
    if_: Condition,

    #[serde(default, skip_serializing_if = "is_default")]
    cpu: CpuDelta,
    #[serde(default, skip_serializing_if = "is_default")]
    power: PowerDelta,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(transparent, default, rename_all = "kebab-case")]
pub struct DaemonConfig(pub Vec<DaemonConfigLayer>);

impl DaemonConfig {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path).with_context(|| {
            format!("failed to read config from '{path}'", path = path.display())
        })?;

        let config: Self = toml::from_str(&contents).context("failed to parse config file")?;

        {
            let mut priorities = Vec::with_capacity(config.0.len());

            for layer in &config.0 {
                if priorities.contains(&layer.priority) {
                    bail!("each config layer must have a different priority")
                }

                priorities.push(layer.priority);
            }
        }

        Ok(config)
    }
}
