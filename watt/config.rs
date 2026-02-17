use std::{
  collections::{
    HashMap,
    HashSet,
    VecDeque,
  },
  fs,
  path::Path,
  sync::Arc,
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
  system,
};

type CpuDeltas = HashMap<Arc<cpu::Cpu>, cpu::Delta>;
type CpuEvalResult = anyhow::Result<(CpuDeltas, Option<bool>)>;
type PowerSupplyDeltas =
  HashMap<Arc<power_supply::PowerSupply>, power_supply::Delta>;
type PowerSupplyEvalResult =
  anyhow::Result<(PowerSupplyDeltas, Option<String>)>;

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
  *value == T::default()
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct CpusDelta {
  /// The CPUs to apply the changes to. When unspecified, will be applied to
  /// all CPUs.
  ///
  /// Type: `Vec<u32>`.
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Expression>,

  /// Set the CPU governor.
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub governor: Option<Expression>,

  /// Set CPU Energy Performance Preference (EPP).
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub energy_performance_preference: Option<Expression>,
  /// Set CPU Energy Performance Bias (EPB).
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub energy_perf_bias:              Option<Expression>,

  /// Set minimum CPU frequency in MHz.
  ///
  /// Type: `u64`.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_mhz_minimum: Option<Expression>,
  /// Set maximum CPU frequency in MHz.
  ///
  /// Type: `u64`.
  #[serde(skip_serializing_if = "is_default")]
  pub frequency_mhz_maximum: Option<Expression>,

  /// Set turbo boost behaviour. Has to be for all CPUs.
  ///
  /// Type: `bool`.
  #[serde(skip_serializing_if = "is_default")]
  pub turbo: Option<Expression>,
}

impl CpusDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> CpuEvalResult {
    log::debug!("evaluating CPU deltas...");

    let cpus = match &self.for_ {
      Some(numbers) => {
        let numbers = numbers
          .eval(state)?
          .context("`cpu.for` resolved to undefined")?;
        let numbers = numbers
          .try_into_list()
          .context("`cpu.for` was not a list")?;

        let mut cpus = Vec::with_capacity(numbers.len());

        for number in numbers {
          let number = number
            .try_into_number()
            .context("`cpu.for` item was not a number")?;

          if number.fract() != 0.0 {
            bail!("invalid CPU in `cpu.for`: {number}");
          }

          cpus.push(number as u32);
        }

        state
          .cpus
          .iter()
          .filter(|cpu| cpus.contains(&cpu.number))
          .cloned()
          .collect()
      },
      None => state.cpus.clone(),
    };

    let mut deltas = HashMap::with_capacity(cpus.len());

    log::trace!("filtering CPUs by number: {cpus:?}");

    for cpu in cpus {
      let state = state.in_context(EvalContext::Cpu(&cpu));
      let mut delta = cpu::Delta::default();

      if let Some(governor) = &self.governor
        && let Some(governor) = governor.eval(&state)?
      {
        let governor = governor
          .try_into_string()
          .context("`cpu.governor` was not a string")?;

        delta.governor = Some(governor);
      }

      if let Some(energy_performance_preference) =
        &self.energy_performance_preference
        && let Some(energy_performance_preference) =
          energy_performance_preference.eval(&state)?
      {
        let energy_performance_preference = energy_performance_preference
          .try_into_string()
          .context("`cpu.energy-performance-preference` was not a string")?;

        delta.energy_performance_preference =
          Some(energy_performance_preference);
      }

      if let Some(energy_perf_bias) = &self.energy_perf_bias
        && let Some(energy_perf_bias) = energy_perf_bias.eval(&state)?
      {
        let energy_perf_bias = energy_perf_bias
          .try_into_string()
          .context("`cpu.energy-perf-bias` was not a string")?;

        delta.energy_perf_bias = Some(energy_perf_bias);
      }

      if let Some(frequency_mhz_minimum) = &self.frequency_mhz_minimum
        && let Some(frequency_mhz_minimum) =
          frequency_mhz_minimum.eval(&state)?
      {
        let frequency_mhz_minimum = frequency_mhz_minimum
          .try_into_number()
          .context("`cpu.frequency-mhz-minimum` was not a number")?;

        let rounded_value = if frequency_mhz_minimum.fract() != 0.0 {
          let rounded = frequency_mhz_minimum.round() as u64;
          log::warn!(
            "`cpu.frequency-mhz-minimum` yielded a float value \
             ({frequency_mhz_minimum}), rounding to {rounded}"
          );
          rounded
        } else {
          frequency_mhz_minimum as u64
        };

        delta.frequency_mhz_minimum = Some(rounded_value);
      }

      if let Some(frequency_mhz_maximum) = &self.frequency_mhz_maximum
        && let Some(frequency_mhz_maximum) =
          frequency_mhz_maximum.eval(&state)?
      {
        let frequency_mhz_maximum = frequency_mhz_maximum
          .try_into_number()
          .context("`cpu.frequency-mhz-maximum` was not a number")?;

        let rounded_value = if frequency_mhz_maximum.fract() != 0.0 {
          let rounded = frequency_mhz_maximum.round() as u64;
          log::warn!(
            "`cpu.frequency-mhz-maximum` yielded a float value \
             ({frequency_mhz_maximum}), rounding to {rounded}"
          );
          rounded
        } else {
          frequency_mhz_maximum as u64
        };

        delta.frequency_mhz_maximum = Some(rounded_value);
      }

      deltas.insert(Arc::clone(&cpu), delta);
    }

    // This is so bad lmao
    let turbo = if let Some(turbo) = &self.turbo
      && let Some(turbo) = turbo.eval(state)?
    {
      let turbo = turbo
        .try_into_boolean()
        .context("`cpu.turbo` was not a boolean")?;

      Some(turbo)
    } else {
      None
    };

    Ok((deltas, turbo))
  }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct PowersDelta {
  /// The power supplies to apply the changes to. When unspecified, will be
  /// applied to all power supplies.
  ///
  /// Type: `Vec<String>`.
  #[serde(rename = "for", skip_serializing_if = "is_default")]
  pub for_: Option<Expression>,

  /// Set the percentage that the power supply has to drop under for charging
  /// to start. Short form: --charge-start.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub charge_threshold_start: Option<Expression>,

  /// Set the percentage where charging will stop. Short form: --charge-end.
  ///
  /// Type: `u8`.
  #[serde(skip_serializing_if = "is_default")]
  pub charge_threshold_end: Option<Expression>,

  /// Set ACPI platform profile. Has to be for all power supplies.
  ///
  /// Type: `String`.
  #[serde(skip_serializing_if = "is_default")]
  pub platform_profile: Option<Expression>,
}

impl PowersDelta {
  pub fn eval(&self, state: &EvalState<'_, '_>) -> PowerSupplyEvalResult {
    log::debug!("evaluating power supply deltas...");

    let power_supplies = match &self.for_ {
      Some(names) => {
        let names = names
          .eval(state)?
          .context("`power.for` resolved to undefined")?;
        let names = names
          .try_into_list()
          .context("`power.for` was not a list")?;

        let mut power_supplies = Vec::with_capacity(names.len());

        for name in names {
          let name = name
            .try_into_string()
            .context("`power.for` item was not a string")?;

          power_supplies.push(name);
        }

        state
          .power_supplies
          .iter()
          .filter(|power_supply| power_supplies.contains(&power_supply.name))
          .cloned()
          .collect()
      },
      None => state.power_supplies.clone(),
    };

    let mut deltas = HashMap::with_capacity(power_supplies.len());

    log::trace!("filtering power supplies by name: {power_supplies:?}");

    for power_supply in power_supplies {
      let state = state.in_context(EvalContext::PowerSupply(&power_supply));
      let mut delta = power_supply::Delta::default();

      if let Some(threshold_start) = &self.charge_threshold_start
        && let Some(threshold_start) = threshold_start.eval(&state)?
      {
        let threshold_start = threshold_start
          .try_into_number()
          .context("`power.charge-threshold-start` was not a number")?;

        delta.charge_threshold_start = Some(threshold_start / 100.0);
      }

      if let Some(threshold_end) = &self.charge_threshold_end
        && let Some(threshold_end) = threshold_end.eval(&state)?
      {
        let threshold_end = threshold_end
          .try_into_number()
          .context("`power.charge-threshold-end` was not a number")?;

        delta.charge_threshold_end = Some(threshold_end / 100.0);
      }

      deltas.insert(Arc::clone(&power_supply), delta);
    }

    let platform_profile = if let Some(platform_profile) =
      &self.platform_profile
      && let Some(platform_profile) = platform_profile.eval(state)?
    {
      let platform_profile = platform_profile
        .try_into_string()
        .context("`power.platform-profile` was not a string")?;

      Some(platform_profile)
    } else {
      None
    };

    Ok((deltas, platform_profile))
  }
}

mod expression {
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

  named!(frequency_available => "?frequency-available");
  named!(turbo_available => "?turbo-available");

  named!(cpu_usage => "%cpu-usage");
  named!(cpu_usage_volatility => "$cpu-usage-volatility");
  named!(cpu_temperature => "$cpu-temperature");
  named!(cpu_temperature_volatility => "$cpu-temperature-volatility");
  named!(cpu_idle_seconds => "$cpu-idle-seconds");
  named!(cpu_frequency_maximum => "$cpu-frequency-maximum");
  named!(cpu_frequency_minimum => "$cpu-frequency-minimum");

  named!(cpu_scaling_maximum => "$cpu-scaling-maximum");

  named!(cpu_core_count => "%cpu-core-count");

  named!(load_average_1m => "$load-average-1m");
  named!(load_average_5m => "$load-average-5m");
  named!(load_average_15m => "$load-average-15m");

  named!(lid_closed => "?lid-closed");

  named!(hour_of_day => "$hour-of-day");

  named!(power_supply_charge => "%power-supply-charge");
  named!(power_supply_discharge_rate => "%power-supply-discharge-rate");

  named!(battery_cycles => "$battery-cycles");
  named!(battery_health => "%battery-health");

  named!(discharging => "?discharging");
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
#[must_use]
pub enum Expression {
  IsGovernorAvailable {
    #[serde(rename = "is-governor-available")]
    value: Box<Expression>,
  },
  IsEnergyPerformancePreferenceAvailable {
    #[serde(rename = "is-energy-performance-preference-available")]
    value: Box<Expression>,
  },
  IsEnergyPerfBiasAvailable {
    #[serde(rename = "is-energy-perf-bias-available")]
    value: Box<Expression>,
  },
  IsPlatformProfileAvailable {
    #[serde(rename = "is-platform-profile-available")]
    value: Box<Expression>,
  },
  IsDriverLoaded {
    #[serde(rename = "is-driver-loaded")]
    value: Box<Expression>,
  },

  #[serde(with = "expression::frequency_available")]
  FrequencyAvailable,

  #[serde(with = "expression::turbo_available")]
  TurboAvailable,

  #[serde(with = "expression::cpu_usage")]
  CpuUsage,

  #[serde(with = "expression::cpu_usage_volatility")]
  CpuUsageVolatility,

  CpuUsageSince {
    #[serde(rename = "cpu-usage-since")]
    duration: String,
  },

  #[serde(with = "expression::cpu_temperature")]
  CpuTemperature,

  #[serde(with = "expression::cpu_temperature_volatility")]
  CpuTemperatureVolatility,

  #[serde(with = "expression::cpu_idle_seconds")]
  CpuIdleSeconds,

  #[serde(with = "expression::cpu_frequency_maximum")]
  CpuFrequencyMaximum,

  #[serde(with = "expression::cpu_frequency_minimum")]
  CpuFrequencyMinimum,

  #[serde(with = "expression::cpu_scaling_maximum")]
  CpuScalingMaximum,

  #[serde(with = "expression::cpu_core_count")]
  CpuCoreCount,

  #[serde(with = "expression::load_average_1m")]
  LoadAverage1m,

  #[serde(with = "expression::load_average_5m")]
  LoadAverage5m,

  #[serde(with = "expression::load_average_15m")]
  LoadAverage15m,

  #[serde(with = "expression::lid_closed")]
  LidClosed,

  #[serde(with = "expression::hour_of_day")]
  HourOfDay,

  #[serde(with = "expression::power_supply_charge")]
  PowerSupplyCharge,

  #[serde(with = "expression::power_supply_discharge_rate")]
  PowerSupplyDischargeRate,

  #[serde(with = "expression::battery_cycles")]
  BatteryCycles,

  #[serde(with = "expression::battery_health")]
  BatteryHealth,

  #[serde(with = "expression::discharging")]
  Discharging,

  Boolean(bool),

  Number(f64),

  String(String),

  List(Vec<Expression>),

  // NUMBER OPERATIONS
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

  Minimum {
    #[serde(rename = "minimum")]
    numbers: Vec<Expression>,
  },
  Maximum {
    #[serde(rename = "maximum")]
    numbers: Vec<Expression>,
  },

  // BOOLEAN OPERATIONS
  IfElse {
    #[serde(rename = "if")]
    condition:   Box<Expression>,
    #[serde(rename = "then")]
    consequence: Box<Expression>,
    #[serde(default, rename = "else", skip_serializing_if = "is_default")]
    alternative: Option<Box<Expression>>,
  },

  IsUnset {
    #[serde(rename = "is-unset")]
    a: Box<Expression>,
  },

  And {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "and")]
    b: Box<Expression>,
  },
  Or {
    #[serde(rename = "value")]
    a: Box<Expression>,
    #[serde(rename = "or")]
    b: Box<Expression>,
  },

  All {
    all: Vec<Expression>,
  },
  Any {
    any: Vec<Expression>,
  },

  Not {
    not: Box<Expression>,
  },

  // OTHER OPERATIONS
  Equal {
    #[serde(rename = "value")]
    a:      Box<Expression>,
    #[serde(rename = "is-equal")]
    b:      Box<Expression>,
    leeway: Box<Expression>,
  },
}

impl Expression {
  pub fn try_into_number(self) -> anyhow::Result<f64> {
    let Self::Number(number) = self else {
      bail!("tried to cast '{self:?}' to a number, failed")
    };

    Ok(number)
  }

  pub fn try_into_boolean(self) -> anyhow::Result<bool> {
    let Self::Boolean(boolean) = self else {
      bail!("tried to cast '{self:?}' to a boolean, failed")
    };

    Ok(boolean)
  }

  pub fn try_into_string(self) -> anyhow::Result<String> {
    let Self::String(string) = self else {
      bail!("tried to cast '{self:?}' to a string, failed")
    };

    Ok(string)
  }

  pub fn try_into_list(self) -> anyhow::Result<Vec<Expression>> {
    let Self::List(list) = self else {
      bail!("tried to cast '{self:?}' to a list, failed")
    };

    Ok(list)
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvalState<'peripherals, 'context> {
  pub frequency_available: bool,
  pub turbo_available:     bool,

  pub cpu_usage:                  f64,
  pub cpu_usage_volatility:       Option<f64>,
  pub cpu_temperature:            Option<f64>,
  pub cpu_temperature_volatility: Option<f64>,
  pub cpu_idle_seconds:           f64,
  pub cpu_frequency_maximum:      Option<f64>,
  pub cpu_frequency_minimum:      Option<f64>,

  pub cpu_scaling_maximum: Option<f64>,

  pub cpu_core_count: u32,

  pub load_average_1m:  f64,
  pub load_average_5m:  f64,
  pub load_average_15m: f64,

  pub lid_closed: bool,

  pub power_supply_charge:         Option<f64>,
  pub power_supply_discharge_rate: Option<f64>,

  pub battery_cycles: Option<f64>,
  pub battery_health: Option<f64>,

  pub discharging: bool,

  pub context: EvalContext<'context>,

  pub cpus:           &'peripherals HashSet<Arc<cpu::Cpu>>,
  pub power_supplies: &'peripherals HashSet<Arc<power_supply::PowerSupply>>,
  pub cpu_log:        &'peripherals VecDeque<system::CpuLog>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalContext<'a> {
  Cpu(&'a cpu::Cpu),
  PowerSupply(&'a power_supply::PowerSupply),
  WidestPossible,
}

impl<'peripherals> EvalState<'peripherals, '_> {
  pub fn in_context<'context>(
    &self,
    context: EvalContext<'context>,
  ) -> EvalState<'peripherals, 'context> {
    EvalState { context, ..*self }
  }
}

impl Expression {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<Expression>> {
    use Expression::*;

    log::trace!("evaluating expression: {self:?}");

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

    Ok(Some(match self {
      IsGovernorAvailable { value } => {
        let value = eval!(value);
        let value = value.try_into_string()?;

        let available = match state.context {
          EvalContext::Cpu(cpu) => cpu.available_governors.contains(&value),
          EvalContext::PowerSupply(_) => false,
          EvalContext::WidestPossible => {
            state
              .cpus
              .iter()
              .any(|cpu| cpu.available_governors.contains(&value))
          },
        };

        Boolean(available)
      },
      IsEnergyPerformancePreferenceAvailable { value } => {
        let value = eval!(value);
        let value = value.try_into_string()?;

        let available = match state.context {
          EvalContext::Cpu(cpu) => cpu.available_epps.contains(&value),
          EvalContext::PowerSupply(_) => false,
          EvalContext::WidestPossible => {
            state
              .cpus
              .iter()
              .any(|cpu| cpu.available_epps.contains(&value))
          },
        };

        Boolean(available)
      },
      IsEnergyPerfBiasAvailable { value } => {
        let value = eval!(value);
        let value = value.try_into_string()?;

        let available = match state.context {
          EvalContext::Cpu(cpu) => cpu.available_epbs.contains(&value),
          EvalContext::PowerSupply(_) => false,
          EvalContext::WidestPossible => {
            state
              .cpus
              .iter()
              .any(|cpu| cpu.available_epbs.contains(&value))
          },
        };

        Boolean(available)
      },
      IsPlatformProfileAvailable { value } => {
        let value = eval!(value);
        let value = value.try_into_string()?;

        let available =
          power_supply::PowerSupply::get_available_platform_profiles()
            .context(
              "failed to get available platform profiles for \
               `is-platform-profile-available`",
            )?
            .contains(&value);

        Boolean(available)
      },
      IsDriverLoaded { value } => {
        let value = eval!(value).try_into_string()?;

        Boolean(crate::fs::exists(format!("/sys/module/{value}")))
      },
      FrequencyAvailable => Boolean(state.frequency_available),
      TurboAvailable => Boolean(state.turbo_available),

      CpuUsage => {
        bail!(
          "`%cpu-usage` is deprecated and has been removed. Use \
           `cpu-usage-since = \"<duration>\"` instead. For example, \
           `cpu-usage-since = \"1sec\"` for CPU usage over the last second."
        )
      },
      CpuUsageSince { duration } => {
        let duration = humantime::parse_duration(duration)
          .with_context(|| format!("failed to parse duration '{duration}'"))?;
        let recent_logs: Vec<&system::CpuLog> = state
          .cpu_log
          .iter()
          .rev()
          .take_while(|log| log.at.elapsed() < duration)
          .collect();

        if recent_logs.len() < 2 {
          // Return None for insufficient data, consistent with volatility
          // expressions
          return Ok(None);
        }

        Number(
          recent_logs.iter().map(|log| log.usage).sum::<f64>()
            / recent_logs.len() as f64,
        )
      },
      CpuUsageVolatility => Number(try_ok!(state.cpu_usage_volatility)),
      CpuTemperature => Number(try_ok!(state.cpu_temperature)),
      CpuTemperatureVolatility => {
        Number(try_ok!(state.cpu_temperature_volatility))
      },
      CpuIdleSeconds => Number(state.cpu_idle_seconds),
      CpuFrequencyMaximum => Number(try_ok!(state.cpu_frequency_maximum)),
      CpuFrequencyMinimum => Number(try_ok!(state.cpu_frequency_minimum)),

      CpuScalingMaximum => Number(try_ok!(state.cpu_scaling_maximum)),

      CpuCoreCount => Number(state.cpu_core_count as f64),

      LoadAverage1m => Number(state.load_average_1m),
      LoadAverage5m => Number(state.load_average_5m),
      LoadAverage15m => Number(state.load_average_15m),

      LidClosed => Boolean(state.lid_closed),

      HourOfDay => {
        let ts = jiff::Timestamp::now()
          .in_tz("local")
          .context("failed to get local timezone for `$hour-of-day`")?;
        Number(ts.hour() as f64)
      },

      PowerSupplyCharge => Number(try_ok!(state.power_supply_charge)),
      PowerSupplyDischargeRate => {
        Number(try_ok!(state.power_supply_discharge_rate))
      },

      BatteryCycles => Number(try_ok!(state.battery_cycles)),
      BatteryHealth => Number(try_ok!(state.battery_health)),

      Discharging => Boolean(state.discharging),

      literal @ (Boolean(_) | Number(_) | String(_)) => literal.clone(),

      List(items) => {
        let mut result = Vec::with_capacity(items.len());

        for item in items {
          result.push(eval!(item));
        }

        List(result)
      },

      Plus { a, b } => {
        Number(eval!(a).try_into_number()? + eval!(b).try_into_number()?)
      },
      Minus { a, b } => {
        Number(eval!(a).try_into_number()? - eval!(b).try_into_number()?)
      },
      Multiply { a, b } => {
        Number(eval!(a).try_into_number()? * eval!(b).try_into_number()?)
      },
      Power { a, b } => {
        Number(
          eval!(a)
            .try_into_number()?
            .powf(eval!(b).try_into_number()?),
        )
      },
      Divide { a, b } => {
        Number(eval!(a).try_into_number()? / eval!(b).try_into_number()?)
      },

      LessThan { a, b } => {
        Boolean(eval!(a).try_into_number()? < eval!(b).try_into_number()?)
      },
      MoreThan { a, b } => {
        Boolean(eval!(a).try_into_number()? > eval!(b).try_into_number()?)
      },

      Minimum { numbers } => {
        let mut evaled = Vec::with_capacity(numbers.len());

        for number in numbers {
          let number = eval!(number).try_into_number()?;
          evaled.push(number);
        }

        Number(
          evaled
            .into_iter()
            .min_by(f64::total_cmp)
            .context("minimum must be given at least 1 expression")?,
        )
      },
      Maximum { numbers } => {
        let mut evaled = Vec::with_capacity(numbers.len());

        for number in numbers {
          let number = eval!(number).try_into_number()?;
          evaled.push(number);
        }

        Number(
          evaled
            .into_iter()
            .max_by(f64::total_cmp)
            .context("maximum must be given at least 1 expression")?,
        )
      },

      IsUnset { a } => Boolean(a.eval(state)?.is_none()),

      IfElse {
        condition,
        consequence,
        alternative,
      } => {
        if eval!(condition).try_into_boolean()? {
          eval!(consequence)
        } else if let Some(alternative) = alternative {
          eval!(alternative)
        } else {
          return Ok(None);
        }
      },

      And { a, b } => {
        Boolean(eval!(a).try_into_boolean()? && eval!(b).try_into_boolean()?)
      },
      Or { a, b } => {
        Boolean(eval!(a).try_into_boolean()? || eval!(b).try_into_boolean()?)
      },

      All { all } => {
        let mut all = all.iter();

        loop {
          let Some(value) = all.next() else {
            break Boolean(true);
          };

          if !eval!(value).try_into_boolean()? {
            break Boolean(false);
          }
        }
      },
      Any { any } => {
        let mut any = any.iter();

        loop {
          let Some(value) = any.next() else {
            break Boolean(false);
          };

          if eval!(value).try_into_boolean()? {
            break Boolean(true);
          }
        }
      },

      Not { not } => Boolean(!eval!(not).try_into_boolean()?),

      Equal { a, b, leeway } => {
        let a = eval!(a).try_into_number()?;
        let b = eval!(b).try_into_number()?;
        let leeway = eval!(leeway).try_into_number()?;

        let minimum = a - leeway;
        let maximum = a + leeway;

        Boolean(minimum < b && b < maximum)
      },
    }))
  }
}

fn literal_true() -> Expression {
  Expression::Boolean(true)
}

fn literal_is_true(expression: &Expression) -> bool {
  expression == &literal_true()
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Rule {
  pub name:     String,
  pub priority: u16,

  #[serde(
    default = "literal_true",
    rename = "if",
    skip_serializing_if = "literal_is_true"
  )]
  pub condition: Expression,

  #[serde(default, skip_serializing_if = "is_default")]
  pub cpu:   CpusDelta,
  #[serde(default, skip_serializing_if = "is_default")]
  pub power: PowersDelta,
}

impl Default for Rule {
  fn default() -> Self {
    Self {
      name:      String::default(),
      priority:  u16::default(),
      condition: literal_true(),
      cpu:       CpusDelta::default(),
      power:     PowersDelta::default(),
    }
  }
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

      log::debug!("validating rule priorities...");

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

    log::debug!("sorted {len} rules by priority", len = config.rules.len());

    log::debug!("loaded config: {config:#?}");

    Ok(config)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use proptest::prelude::*;

  use super::*;

  proptest! {
    #[test]
    fn test_multiply_float(
      base_freq in 1000u64..10000u64,
      multiplier in 0.1f64..1.0f64
    ) {
      // Create a mock CPU with all required fields
      // FIXME: figure out if there is a less ugly way of testing this and
      // share CPU state across tests
      let cpu = Arc::new(cpu::Cpu {
        number: 0,
        has_cpufreq: true,
        available_governors: vec![],
        governor: None,
        frequency_mhz: Some(base_freq),
        frequency_mhz_minimum: Some(1000),
        frequency_mhz_maximum: Some(base_freq),
        available_epps: vec![],
        epp: None,
        available_epbs: vec![],
        epb: None,
        stat: cpu::CpuStat::default(),
        previous_stat: None,
        info: None,
      });

      let mut cpus = HashSet::new();
      cpus.insert(cpu.clone());

      let power_supplies = HashSet::new();
      let cpu_log = VecDeque::new();

      // Create an eval state with the base frequency
      let state = EvalState {
        frequency_available: true,
        turbo_available: false,
        cpu_usage: 0.5,
        cpu_usage_volatility: Some(0.1),
        cpu_temperature: Some(50.0),
        cpu_temperature_volatility: Some(5.0),
        cpu_idle_seconds: 10.0,
        cpu_frequency_maximum: Some(base_freq as f64),
        cpu_frequency_minimum: Some(1000.0),
        cpu_scaling_maximum: Some(base_freq as f64),
        cpu_core_count: 1,
        load_average_1m: 0.5,
        load_average_5m: 0.6,
        load_average_15m: 0.7,
        lid_closed: false,
        power_supply_charge: Some(0.8),
        power_supply_discharge_rate: Some(10.0),
        battery_cycles: Some(100.0),
        battery_health: Some(0.95),
        discharging: false,
        context: EvalContext::Cpu(&cpu),
        cpus: &cpus,
        power_supplies: &power_supplies,
        cpu_log: &cpu_log,
      };

      // Create an expression like: { value = "$cpu-frequency-maximum", multiply = 0.65 }
      let expr = Expression::Multiply {
        a: Box::new(Expression::CpuFrequencyMaximum),
        b: Box::new(Expression::Number(multiplier)),
      };

      // Evaluate the expression
      let result = expr.eval(&state);

      // Before the fix, this would succeed but then crash when converting to u64
      // After the fix, this should succeed and round the result
      prop_assert!(result.is_ok());

      if let Ok(Some(Expression::Number(value))) = result {
        // The result might be a float
        let _expected_float = base_freq as f64 * multiplier;

        // Create a CpusDelta with the frequency_mhz_maximum field
        let cpu_delta = CpusDelta {
          for_: None,
          governor: None,
          energy_performance_preference: None,
          energy_perf_bias: None,
          frequency_mhz_minimum: None,
          frequency_mhz_maximum: Some(Expression::Number(value)),
          turbo: None,
        };

        // Try to evaluate it - this should not panic after the fix
        let eval_result = cpu_delta.eval(&state);

        // This test should pass after the fix is applied
        prop_assert!(
          eval_result.is_ok(),
          "Evaluation should succeed with float frequency values"
        );
      }
    }
  }

  // Specific test case that would have crashed before the fix
  // Example: 5000 MHz * 0.65 = 3250.0 (no fractional part, but it's a float)
  // Example: 3333 MHz * 0.65 = 2166.45 (has fractional part)
  #[test]
  fn test_rounding() {
    let cpu = Arc::new(cpu::Cpu {
      number:                0,
      has_cpufreq:           true,
      available_governors:   vec![],
      governor:              None,
      frequency_mhz:         Some(3333),
      frequency_mhz_minimum: Some(1000),
      frequency_mhz_maximum: Some(3333),
      available_epps:        vec![],
      epp:                   None,
      available_epbs:        vec![],
      epb:                   None,
      stat:                  cpu::CpuStat::default(),
      previous_stat:         None,
      info:                  None,
    });

    let mut cpus = HashSet::new();
    cpus.insert(cpu.clone());

    let power_supplies = HashSet::new();
    let cpu_log = VecDeque::new();

    let state = EvalState {
      frequency_available:         true,
      turbo_available:             false,
      cpu_usage:                   0.5,
      cpu_usage_volatility:        Some(0.1),
      cpu_temperature:             Some(50.0),
      cpu_temperature_volatility:  Some(5.0),
      cpu_idle_seconds:            10.0,
      cpu_frequency_maximum:       Some(3333.0),
      cpu_frequency_minimum:       Some(1000.0),
      cpu_scaling_maximum:         Some(3500.0),
      cpu_core_count:              1,
      load_average_1m:             0.5,
      load_average_5m:             0.6,
      load_average_15m:            0.7,
      lid_closed:                  false,
      power_supply_charge:         Some(0.8),
      power_supply_discharge_rate: Some(10.0),
      battery_cycles:              Some(100.0),
      battery_health:              Some(0.95),
      discharging:                 false,
      context:                     EvalContext::Cpu(&cpu),
      cpus:                        &cpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };

    // 3333 * 0.65 = 2166.45
    let cpu_delta = CpusDelta {
      for_:                          None,
      governor:                      None,
      energy_performance_preference: None,
      energy_perf_bias:              None,
      frequency_mhz_minimum:         None,
      frequency_mhz_maximum:         Some(Expression::Multiply {
        a: Box::new(Expression::CpuFrequencyMaximum),
        b: Box::new(Expression::Number(0.65)),
      }),
      turbo:                         None,
    };

    // Previously this would bail! with "invalid number for ...". With the
    // rounding changes this should succeed, and round to 2166
    let result = cpu_delta.eval(&state);

    assert!(
      result.is_ok(),
      "Should handle float results from multiplication"
    );

    if let Ok((deltas, _)) = result {
      let delta = deltas.get(&cpu).unwrap();
      assert!(delta.frequency_mhz_maximum.is_some());
      let freq = delta.frequency_mhz_maximum.unwrap();
      assert_eq!(freq, 2166); // should be rounded to 2166
    }
  }

  #[test]
  fn test_volatility_expressions_with_insufficient_data() {
    let cpu = Arc::new(cpu::Cpu {
      number:                0,
      has_cpufreq:           true,
      available_governors:   vec![],
      governor:              None,
      frequency_mhz:         Some(3333),
      frequency_mhz_minimum: Some(1000),
      frequency_mhz_maximum: Some(3333),
      available_epps:        vec![],
      epp:                   None,
      available_epbs:        vec![],
      epb:                   None,
      stat:                  cpu::CpuStat::default(),
      previous_stat:         None,
      info:                  None,
    });

    let mut cpus = HashSet::new();
    cpus.insert(cpu.clone());

    let power_supplies = HashSet::new();
    let cpu_log = VecDeque::new();

    let state = EvalState {
      frequency_available:         true,
      turbo_available:             false,
      cpu_usage:                   0.0,
      cpu_usage_volatility:        None,
      cpu_temperature:             None,
      cpu_temperature_volatility:  None,
      cpu_idle_seconds:            0.0,
      cpu_frequency_maximum:       Some(3333.0),
      cpu_frequency_minimum:       Some(1000.0),
      cpu_scaling_maximum:         Some(3500.0),
      cpu_core_count:              1,
      load_average_1m:             0.0,
      load_average_5m:             0.0,
      load_average_15m:            0.0,
      lid_closed:                  false,
      power_supply_charge:         None,
      power_supply_discharge_rate: None,
      battery_cycles:              None,
      battery_health:              None,
      discharging:                 false,
      context:                     EvalContext::Cpu(&cpu),
      cpus:                        &cpus,
      power_supplies:              &power_supplies,
      cpu_log:                     &cpu_log,
    };

    let result = Expression::CpuUsageVolatility.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuUsageVolatility should return None with insufficient data"
    );

    let result = Expression::CpuTemperature.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuTemperature should return None with insufficient data"
    );

    let result = Expression::CpuTemperatureVolatility.eval(&state);
    assert!(
      result.is_ok() && result.as_ref().unwrap().is_none(),
      "CpuTemperatureVolatility should return None with insufficient data"
    );
  }
}
