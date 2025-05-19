use std::{fs, path::Path};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::{cpu, power_supply};

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

impl CpuDelta {
    pub fn apply(&self) -> anyhow::Result<()> {
        let cpus = match &self.for_ {
            Some(numbers) => {
                let mut cpus = Vec::with_capacity(numbers.len());

                for &number in numbers {
                    cpus.push(cpu::Cpu::new(number)?);
                }

                cpus
            }
            None => cpu::Cpu::all().context("failed to get all CPUs and their information")?,
        };

        for cpu in cpus {
            if let Some(governor) = self.governor.as_ref() {
                cpu.set_governor(governor)?;
            }

            if let Some(epp) = self.energy_performance_preference.as_ref() {
                cpu.set_epp(epp)?;
            }

            if let Some(epb) = self.energy_performance_bias.as_ref() {
                cpu.set_epb(epb)?;
            }

            if let Some(mhz_minimum) = self.frequency_mhz_minimum {
                cpu.set_frequency_minimum(mhz_minimum)?;
            }

            if let Some(mhz_maximum) = self.frequency_mhz_maximum {
                cpu.set_frequency_maximum(mhz_maximum)?;
            }
        }

        if let Some(turbo) = self.turbo {
            cpu::Cpu::set_turbo(turbo)?;
        }

        Ok(())
    }
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

impl PowerDelta {
    pub fn apply(&self) -> anyhow::Result<()> {
        let power_supplies = match &self.for_ {
            Some(names) => {
                let mut power_supplies = Vec::with_capacity(names.len());

                for name in names {
                    power_supplies.push(power_supply::PowerSupply::from_name(name.clone())?);
                }

                power_supplies
            }

            None => power_supply::PowerSupply::all()?
                .into_iter()
                .filter(|power_supply| power_supply.threshold_config.is_some())
                .collect(),
        };

        for power_supply in power_supplies {
            if let Some(threshold_start) = self.charge_threshold_start {
                power_supply.set_charge_threshold_start(threshold_start)?;
            }

            if let Some(threshold_end) = self.charge_threshold_end {
                power_supply.set_charge_threshold_end(threshold_end)?;
            }
        }

        if let Some(platform_profile) = self.platform_profile.as_ref() {
            power_supply::PowerSupply::set_platform_profile(platform_profile)?;
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(untagged, rename_all = "kebab-case")]
pub enum Expression {
    #[serde(rename = "$cpu-temperature")]
    CpuTemperature,

    #[serde(rename = "%cpu-volatility")]
    CpuVolatility,

    #[serde(rename = "%cpu-utilization")]
    CpuUtilization,

    #[serde(rename = "%power-supply-charge")]
    PowerSupplyCharge,

    #[serde(rename = "%power-supply-discharge-rate")]
    PowerSupplyDischargeRate,

    #[serde(rename = "?charging")]
    Charging,
    #[serde(rename = "?on-battery")]
    OnBattery,

    #[serde(rename = "#false")]
    False,

    #[default]
    #[serde(rename = "#true")]
    True,

    Number(f64),

    Plus {
        value: Box<Expression>,
        plus: Box<Expression>,
    },
    Minus {
        value: Box<Expression>,
        minus: Box<Expression>,
    },
    Multiply {
        value: Box<Expression>,
        multiply: Box<Expression>,
    },
    Power {
        value: Box<Expression>,
        power: Box<Expression>,
    },
    Divide {
        value: Box<Expression>,
        divide: Box<Expression>,
    },

    LessThan {
        value: Box<Expression>,
        is_less_than: Box<Expression>,
    },

    MoreThan {
        value: Box<Expression>,
        is_more_than: Box<Expression>,
    },

    Equal {
        value: Box<Expression>,
        is_equal: Box<Expression>,
        leeway: Box<Expression>,
    },

    And {
        value: Box<Expression>,
        and: Box<Expression>,
    },
    All {
        all: Vec<Expression>,
    },

    Or {
        value: Box<Expression>,
        or: Box<Expression>,
    },
    Any {
        any: Vec<Expression>,
    },

    Not {
        not: Box<Expression>,
    },
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Rule {
    priority: u8,

    #[serde(default, skip_serializing_if = "is_default")]
    if_: Expression,

    #[serde(default, skip_serializing_if = "is_default")]
    cpu: CpuDelta,
    #[serde(default, skip_serializing_if = "is_default")]
    power: PowerDelta,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(transparent, default, rename_all = "kebab-case")]
pub struct DaemonConfig {
    #[serde(rename = "rule")]
    rules: Vec<Rule>,
}

impl DaemonConfig {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path).with_context(|| {
            format!("failed to read config from '{path}'", path = path.display())
        })?;

        let config: Self = toml::from_str(&contents).context("failed to parse config file")?;

        {
            let mut priorities = Vec::with_capacity(config.rules.len());

            for rule in &config.rules {
                if priorities.contains(&rule.priority) {
                    bail!("each config rule must have a different priority")
                }

                priorities.push(rule.priority);
            }
        }

        Ok(config)
    }
}
