use std::{
  cmp::Ordering,
  time::Instant,
};

use anyhow::{
  Context as _,
  bail,
};
use humantime::parse_duration;
use jiff::Timestamp;

use super::{
  EvalContext,
  EvalState,
  Expression,
  find_battery,
};
use crate::{
  fs,
  power_supply::PowerSupply,
  system::CpuLog,
};

impl Expression {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<Self>> {
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
        let value = eval!(value).try_into_string()?;
        Boolean(
          state.cpu_value_available(&value, |cpu| &cpu.available_governors),
        )
      },
      IsEnergyPerformancePreferenceAvailable { value } => {
        let value = eval!(value).try_into_string()?;
        Boolean(state.cpu_value_available(&value, |cpu| &cpu.available_epps))
      },
      IsEnergyPerfBiasAvailable { value } => {
        let value = eval!(value).try_into_string()?;
        Boolean(state.cpu_value_available(&value, |cpu| &cpu.available_epbs))
      },
      IsPlatformProfileAvailable { value } => {
        let value = eval!(value).try_into_string()?;

        let available = PowerSupply::get_available_platform_profiles()
          .context(
            "failed to get available platform profiles for \
             `is-platform-profile-available`",
          )?
          .contains(&value);

        Boolean(available)
      },
      IsChassisType { value } => {
        let value = eval!(value).try_into_string()?;

        Boolean(state.chassis_type == Some(value.as_str()))
      },
      FirstAvailableGovernor { values } => {
        let values =
          try_ok!(eval_string_list(values, state, "first-available-governor")?);

        let Some(governor) = values.iter().find(|value| {
          state.cpu_value_available(value, |cpu| &cpu.available_governors)
        }) else {
          log::debug!(
            "none of the requested governors are available: {governors}",
            governors = values.join(", "),
          );
          return Ok(None);
        };

        String(governor.clone())
      },
      FirstAvailableEnergyPerformancePreference { values } => {
        let values = try_ok!(eval_string_list(
          values,
          state,
          "first-available-energy-performance-preference",
        )?);

        String(try_ok!(values.into_iter().find(|value| {
          state.cpu_value_available(value, |cpu| &cpu.available_epps)
        })))
      },
      FirstAvailableEnergyPerfBias { values } => {
        let values = try_ok!(eval_string_list(
          values,
          state,
          "first-available-energy-perf-bias",
        )?);

        String(try_ok!(values.into_iter().find(|value| {
          state.cpu_value_available(value, |cpu| &cpu.available_epbs)
        })))
      },
      FirstAvailablePlatformProfile { values } => {
        let values = try_ok!(eval_string_list(
          values,
          state,
          "first-available-platform-profile",
        )?);

        let available = PowerSupply::get_available_platform_profiles()
          .context(
            "failed to get available platform profiles for \
             `first-available-platform-profile`",
          )?;

        String(try_ok!(
          values.into_iter().find(|value| available.contains(value))
        ))
      },
      IsDriverLoaded { value } => {
        let value = eval!(value).try_into_string()?;

        Boolean(fs::exists(format!("/sys/module/{value}")))
      },
      IsBatteryAvailable { value } => {
        let value = eval!(value).try_into_string()?;

        Boolean(find_battery(state.power_supplies, &value).is_some())
      },
      FrequencyAvailable => {
        Boolean(match state.context {
          EvalContext::Cpu(cpu) => cpu.frequency_available(),
          EvalContext::PowerSupply(_) => false,
          EvalContext::WidestPossible => state.frequency_available,
        })
      },
      TurboAvailable => Boolean(state.turbo_available),

      CpuUsage => {
        bail!(
          "`%cpu-usage` is deprecated and has been removed. Use \
           `cpu-usage-since = \"<duration>\"` instead. For example, \
           `cpu-usage-since = \"1sec\"` for CPU usage over the last second."
        )
      },
      CpuUsageSince { duration } => {
        Number(try_ok!(average_since(state, eval!(duration), |log| {
          log.usage
        })?))
      },
      CpuUsageVolatility => Number(try_ok!(state.cpu_usage_volatility)),
      CpuTemperature => Number(try_ok!(state.cpu_temperature)),
      CpuTemperatureVolatility => {
        Number(try_ok!(state.cpu_temperature_volatility))
      },
      CpuIdleSeconds => Number(state.cpu_idle_seconds),
      CpuFrequencyMaximum => {
        Number(try_ok!(match state.context {
          EvalContext::Cpu(cpu) => cpu.frequency_maximum.map(|f| f.as_mhz()),
          EvalContext::PowerSupply(_) => None,
          EvalContext::WidestPossible => state.cpu_frequency_maximum,
        }))
      },
      CpuFrequencyMinimum => {
        Number(try_ok!(match state.context {
          EvalContext::Cpu(cpu) => cpu.frequency_minimum.map(|f| f.as_mhz()),
          EvalContext::PowerSupply(_) => None,
          EvalContext::WidestPossible => state.cpu_frequency_minimum,
        }))
      },

      CpuScalingMaximum => {
        let max = state
          .cpus
          .iter()
          .filter_map(|cpu| cpu.frequency_maximum)
          .max()
          .map(|frequency| frequency.as_mhz());
        Number(try_ok!(max))
      },

      CpuCoreCount => Number(state.cpus.len() as f64),

      LoadAverageSince { duration } => {
        Number(try_ok!(average_since(state, eval!(duration), |log| {
          log.load_average
        })?))
      },

      LidClosed => Boolean(state.lid_closed),
      VirtualMachine => Boolean(state.virtual_machine),

      HourOfDay => {
        let ts = Timestamp::now()
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

      BatteryCyclesFor { name } => {
        let battery = find_battery(state.power_supplies, name);
        Number(try_ok!(battery.and_then(|ps| ps.cycles).map(|c| c as f64)))
      },

      BatteryHealthFor { name } => {
        let battery = find_battery(state.power_supplies, name);
        Number(try_ok!(battery.and_then(|ps| ps.health)))
      },

      Discharging => Boolean(state.discharging),

      PowerProfilePreference => {
        String(state.power_profile_preference.as_str().to_owned())
      },

      literal @ (Boolean(_) | Number(_) | String(_)) => literal.clone(),

      List(items) => {
        let mut result = Vec::with_capacity(items.len());

        for item in items {
          result.push(eval!(item));
        }

        List(result)
      },

      Plus { a, b }
      | Minus { a, b }
      | Multiply { a, b }
      | Power { a, b }
      | Divide { a, b } => {
        let a = eval!(a).try_into_number()?;
        let b = eval!(b).try_into_number()?;
        Number(self.calculate_arithmetic(a, b)?)
      },

      LessThan { a, b } | MoreThan { a, b } => {
        let a = eval!(a).try_into_number()?;
        let b = eval!(b).try_into_number()?;
        Boolean(self.calculate_comparison(a, b, 0.0))
      },
      Equal { a, b, leeway } => {
        let a = eval!(a).try_into_number()?;
        let b = eval!(b).try_into_number()?;
        let leeway = eval!(leeway).try_into_number()?;
        Boolean(self.calculate_comparison(a, b, leeway))
      },

      Minimum { numbers } | Maximum { numbers } => {
        let (name, compare): (_, fn(&f64, &f64) -> Ordering) =
          if matches!(self, Minimum { .. }) {
            ("minimum", f64::total_cmp)
          } else {
            ("maximum", |a, b| b.total_cmp(a))
          };

        let mut result = None;
        for number in numbers {
          let number = eval!(number).try_into_number()?;
          result = Some(Self::select_extreme(number, result, compare));
        }

        Number(result.with_context(|| {
          format!("{name} must be given at least 1 expression")
        })?)
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
    }))
  }

  pub(super) fn calculate_arithmetic(
    &self,
    a: f64,
    b: f64,
  ) -> anyhow::Result<f64> {
    let result = match self {
      Self::Plus { .. } => a + b,
      Self::Minus { .. } => a - b,
      Self::Multiply { .. } => a * b,
      Self::Power { .. } => a.powf(b),
      Self::Divide { .. } => a / b,
      _ => unreachable!("not an arithmetic expression"),
    };
    if !result.is_finite() {
      bail!("arithmetic expression produced a non-finite number");
    }
    Ok(result)
  }

  pub(super) fn calculate_comparison(
    &self,
    a: f64,
    b: f64,
    leeway: f64,
  ) -> bool {
    match self {
      Self::LessThan { .. } => a < b,
      Self::MoreThan { .. } => a > b,
      Self::Equal { .. } => a - leeway < b && b < a + leeway,
      _ => unreachable!("not a comparison expression"),
    }
  }

  pub(super) fn select_extreme(
    number: f64,
    previous: Option<f64>,
    compare: impl Fn(&f64, &f64) -> Ordering,
  ) -> f64 {
    match previous {
      Some(previous) if compare(&number, &previous).is_ge() => previous,
      _ => number,
    }
  }
}

fn eval_string_list(
  values: &[Expression],
  state: &EvalState<'_, '_>,
  name: &str,
) -> anyhow::Result<Option<Vec<String>>> {
  let mut strings = Vec::with_capacity(values.len());

  for value in values {
    let Some(value) = value.eval(state)? else {
      return Ok(None);
    };

    strings.push(
      value
        .try_into_string()
        .with_context(|| format!("`{name}` item was not a string"))?,
    );
  }

  Ok(Some(strings))
}

fn average_since(
  state: &EvalState<'_, '_>,
  duration: Expression,
  value: impl Fn(&CpuLog) -> f64,
) -> anyhow::Result<Option<f64>> {
  let duration = duration.try_into_string()?;
  let duration = parse_duration(&duration)
    .with_context(|| format!("failed to parse duration '{duration}'"))?;

  let (total, count) = CpuLog::recent(state.cpu_log, Instant::now(), duration)
    .fold((0.0, 0usize), |(total, count), log| {
      (total + value(log), count + 1)
    });

  // Each sample measures an interval, so one sample suffices for a short
  // window.
  Ok((count > 0).then(|| total / count as f64))
}
