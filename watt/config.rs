use std::{
  fs,
  path::Path,
};

use anyhow::{
  Context,
  bail,
};
use serde::{
  Deserialize,
  Serialize,
};

use crate::{
  cpu,
  power_supply,
};

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
  *value == T::default()
}

#[derive(
  Serialize, Deserialize, clap::Parser, Default, Debug, Clone, PartialEq, Eq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct CpuDelta {
  /// The CPUs to apply the changes to. When unspecified, will be applied to
  /// all CPUs.
  #[arg(short = 'c', long = "for")]
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Vec<u32>>,

  /// Set the CPU governor.
  #[arg(short = 'g', long)]
  #[serde(skip_serializing_if = "is_default")]
  pub governor: Option<String>, /* TODO: Validate with clap for available
                                 * governors. */

  /// Set CPU Energy Performance Preference (EPP). Short form: --epp.
  #[arg(short = 'p', long, alias = "epp")]
  #[serde(skip_serializing_if = "is_default")]
  pub energy_performance_preference: Option<String>, /* TODO: Validate with
                                                      * clap for available
                                                      * governors. */

  /// Set CPU Energy Performance Bias (EPB). Short form: --epb.
  #[arg(short = 'b', long, alias = "epb")]
  #[serde(skip_serializing_if = "is_default")]
  pub energy_performance_bias: Option<String>, /* TODO: Validate with clap for available governors. */

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
    let mut cpus = match &self.for_ {
      Some(numbers) => {
        let mut cpus = Vec::with_capacity(numbers.len());
        let cache = cpu::CpuRescanCache::default();

        for &number in numbers {
          cpus.push(cpu::Cpu::new(number, &cache)?);
        }

        cpus
      },
      None => {
        cpu::Cpu::all()
          .context("failed to get all CPUs and their information")?
      },
    };

    for cpu in &mut cpus {
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
        cpu.set_frequency_mhz_minimum(mhz_minimum)?;
      }

      if let Some(mhz_maximum) = self.frequency_mhz_maximum {
        cpu.set_frequency_mhz_maximum(mhz_maximum)?;
      }
    }

    if let Some(turbo) = self.turbo {
      cpu::Cpu::set_turbo(turbo)?;
    }

    Ok(())
  }
}

#[derive(
  Serialize, Deserialize, clap::Parser, Default, Debug, Clone, PartialEq, Eq,
)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct PowerDelta {
  /// The power supplies to apply the changes to. When unspecified, will be
  /// applied to all power supplies.
  #[arg(short = 'p', long = "for")]
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Vec<String>>,

  /// Set the percentage that the power supply has to drop under for charging
  /// to start. Short form: --charge-start.
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
    let mut power_supplies = match &self.for_ {
      Some(names) => {
        let mut power_supplies = Vec::with_capacity(names.len());

        for name in names {
          power_supplies
            .push(power_supply::PowerSupply::from_name(name.clone())?);
        }

        power_supplies
      },

      None => {
        power_supply::PowerSupply::all()?
          .into_iter()
          .filter(|power_supply| power_supply.threshold_config.is_some())
          .collect()
      },
    };

    for power_supply in &mut power_supplies {
      if let Some(threshold_start) = self.charge_threshold_start {
        power_supply
          .set_charge_threshold_start(threshold_start as f64 / 100.0)?;
      }

      if let Some(threshold_end) = self.charge_threshold_end {
        power_supply.set_charge_threshold_end(threshold_end as f64 / 100.0)?;
      }
    }

    if let Some(platform_profile) = self.platform_profile.as_ref() {
      power_supply::PowerSupply::set_platform_profile(platform_profile)?;
    }

    Ok(())
  }
}

macro_rules! named {
  ($variant:ident => $value:literal) => {
    pub mod $variant {
      pub fn serialize<S: serde::Serializer>(
        serializer: S,
      ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str($value)
      }

      pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
      ) -> Result<(), D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
          type Value = ();

          fn expecting(
            &self,
            writer: &mut std::fmt::Formatter,
          ) -> std::fmt::Result {
            writer.write_str(concat!("\"", $value, "\""))
          }

          fn visit_str<E: serde::de::Error>(
            self,
            value: &str,
          ) -> Result<Self::Value, E> {
            if value != $value {
              return Err(E::invalid_value(
                serde::de::Unexpected::Str(value),
                &self,
              ));
            }

            Ok(())
          }
        }

        deserializer.deserialize_str(Visitor)
      }
    }
  };
}

mod expression {
  named!(cpu_usage => "%cpu-usage");
  named!(cpu_usage_volatility => "$cpu-usage-volatility");
  named!(cpu_temperature => "$cpu-temperature");
  named!(cpu_temperature_volatility => "$cpu-temperature-volatility");
  named!(cpu_idle_seconds => "$cpu-idle-seconds");

  named!(power_supply_charge => "%power-supply-charge");
  named!(power_supply_discharge_rate => "%power-supply-discharge-rate");

  named!(discharging => "?discharging");
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Expression {
  #[serde(with = "expression::cpu_usage")]
  CpuUsage,

  #[serde(with = "expression::cpu_usage_volatility")]
  CpuUsageVolatility,

  #[serde(with = "expression::cpu_temperature")]
  CpuTemperature,

  #[serde(with = "expression::cpu_temperature_volatility")]
  CpuTemperatureVolatility,

  #[serde(with = "expression::cpu_idle_seconds")]
  CpuIdleSeconds,

  #[serde(with = "expression::power_supply_charge")]
  PowerSupplyCharge,

  #[serde(with = "expression::power_supply_discharge_rate")]
  PowerSupplyDischargeRate,

  #[serde(with = "expression::discharging")]
  Discharging,

  Boolean(bool),

  Number(f64),

  Plus {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "plus")]
    b: Box<Expression>,
  },
  Minus {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "minus")]
    b: Box<Expression>,
  },
  Multiply {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "multiply")]
    b: Box<Expression>,
  },
  Power {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "power")]
    b: Box<Expression>,
  },
  Divide {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "divide")]
    b: Box<Expression>,
  },

  LessThan {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "is-less-than")]
    b: Box<Expression>,
  },
  MoreThan {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "is-more-than")]
    b: Box<Expression>,
  },

  Equal {
    #[serde(rename = "value")]
    a:      Box<Expression>,
    #[serde(rename = "is-equal")]
    b:      Box<Expression>,
    leeway: Box<Expression>,
  },

  And {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "and")]
    b: Box<Expression>,
  },
  All {
    all: Vec<Expression>,
  },

  Or {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "or")]
    b: Box<Expression>,
  },
  Any {
    any: Vec<Expression>,
  },

  Not {
    not: Box<Expression>,
  },
}

impl Default for Expression {
  fn default() -> Self {
    Self::Boolean(true)
  }
}

impl Expression {
  pub fn as_number(&self) -> anyhow::Result<f64> {
    let Self::Number(number) = self else {
      bail!("tried to cast '{self:?}' to a number, failed")
    };

    Ok(*number)
  }

  pub fn as_boolean(&self) -> anyhow::Result<bool> {
    let Self::Boolean(boolean) = self else {
      bail!("tried to cast '{self:?}' to a boolean, failed")
    };

    Ok(*boolean)
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalState {
  pub cpu_usage:                  f64,
  pub cpu_usage_volatility:       Option<f64>,
  pub cpu_temperature:            f64,
  pub cpu_temperature_volatility: Option<f64>,
  pub cpu_idle_seconds:           f64,

  pub power_supply_charge:         f64,
  pub power_supply_discharge_rate: Option<f64>,

  pub discharging: bool,
}

impl Expression {
  pub fn eval(&self, state: &EvalState) -> anyhow::Result<Option<Expression>> {
    use Expression::*;

    macro_rules! try_ok {
      ($expression:expr) => {
        match $expression {
          Some(value) => value,
          None => return Ok(None),
        }
      };
    }

    macro_rules! eval {
      ($expression:expr) => {
        try_ok!($expression.eval(state)?)
      };
    }

    // [e8dax09]: This may be look inefficient, and it definitely isn't optimal,
    // but expressions in rules are usually so small that it doesn't matter or
    // make a perceiveable performance difference.
    //
    // We also want to be strict, instead of lazy in binary operations, because
    // we want to catch type errors immediately.
    //
    // FIXME: We currently cannot catch errors that will happen when propagating
    // None. You can have a type error go uncaught on first startup by using
    // $cpu-usage-volatility incorrectly, for example.
    Ok(Some(match self {
      CpuUsage => Number(state.cpu_usage),
      CpuUsageVolatility => Number(try_ok!(state.cpu_usage_volatility)),
      CpuTemperature => Number(state.cpu_temperature),
      CpuTemperatureVolatility => {
        Number(try_ok!(state.cpu_temperature_volatility))
      },
      CpuIdleSeconds => Number(state.cpu_idle_seconds),

      PowerSupplyCharge => Number(state.power_supply_charge),
      PowerSupplyDischargeRate => {
        Number(try_ok!(state.power_supply_discharge_rate))
      },

      Discharging => Boolean(state.discharging),

      literal @ (Boolean(_) | Number(_)) => literal.clone(),

      Plus { a, b } => Number(eval!(a).as_number()? + eval!(b).as_number()?),
      Minus { a, b } => Number(eval!(a).as_number()? - eval!(b).as_number()?),
      Multiply { a, b } => {
        Number(eval!(a).as_number()? * eval!(b).as_number()?)
      },
      Power { a, b } => {
        Number(eval!(a).as_number()?.powf(eval!(b).as_number()?))
      },
      Divide { a, b } => Number(eval!(a).as_number()? / eval!(b).as_number()?),

      LessThan { a, b } => {
        Boolean(eval!(a).as_number()? < eval!(b).as_number()?)
      },
      MoreThan { a, b } => {
        Boolean(eval!(a).as_number()? > eval!(b).as_number()?)
      },
      Equal { a, b, leeway } => {
        let a = eval!(a).as_number()?;
        let b = eval!(b).as_number()?;
        let leeway = eval!(leeway).as_number()?;

        let minimum = a - leeway;
        let maximum = a + leeway;

        Boolean(minimum < b && b < maximum)
      },

      And { a, b } => {
        let a = eval!(a).as_boolean()?;
        let b = eval!(b).as_boolean()?;

        Boolean(a && b)
      },
      All { all } => {
        let mut result = true;

        for value in all {
          let value = eval!(value).as_boolean()?;

          result = result && value;
        }

        Boolean(result)
      },
      Or { a, b } => {
        let a = eval!(a).as_boolean()?;
        let b = eval!(b).as_boolean()?;

        Boolean(a || b)
      },
      Any { any } => {
        let mut result = false;

        for value in any {
          let value = eval!(value).as_boolean()?;

          result = result || value;
        }

        Boolean(result)
      },
      Not { not } => Boolean(!eval!(not).as_boolean()?),
    }))
  }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Rule {
  pub priority: u16,

  #[serde(default, rename = "if", skip_serializing_if = "is_default")]
  pub condition: Expression,

  #[serde(default, skip_serializing_if = "is_default")]
  pub cpu:   CpuDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub power: PowerDelta,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DaemonConfig {
  #[serde(rename = "rule")]
  pub rules: Vec<Rule>,
}

impl DaemonConfig {
  const DEFAULT: &str = include_str!("config.toml");

  pub fn load_from(path: Option<&Path>) -> anyhow::Result<Self> {
    let contents = if let Some(path) = path {
      log::info!("loading config from '{path}'", path = path.display());

      &fs::read_to_string(path).with_context(|| {
        format!("failed to read config from '{path}'", path = path.display())
      })?
    } else {
      log::info!("loading default config");

      Self::DEFAULT
    };

    let mut config: Self = toml::from_str(contents).with_context(|| {
      path.map_or(
        "failed to parse builtin default config, this is a bug".to_owned(),
        |p| format!("failed to parse file at '{path}'", path = p.display()),
      )
    })?;

    {
      let mut priorities = Vec::with_capacity(config.rules.len());

      for rule in &config.rules {
        if priorities.contains(&rule.priority) {
          bail!("each config rule must have a different priority")
        }

        priorities.push(rule.priority);
      }
    }

    // This is just for debug traces.
    if log::max_level() >= log::LevelFilter::Debug {
      if config.rules.is_sorted_by_key(|rule| rule.priority) {
        log::debug!(
          "config rules are sorted by increasing priority, not doing anything"
        );
      } else {
        log::debug!("config rules aren't sorted by priority, sorting");
      }
    }

    config.rules.sort_by_key(|rule| rule.priority);

    log::debug!("loaded config: {config:#?}");

    Ok(config)
  }
}
