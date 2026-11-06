use anyhow::{
  Context as _,
  bail,
};
use humantime::parse_duration;
use jiff::Timestamp;
use watt_platform::{
  fs,
  power_supply::PowerSupply,
};

use super::{
  EvalContext,
  EvalState,
  Expression,
  find_battery,
};
use crate::CpuLog;

pub(super) enum Evaluated<T> {
  Value(T),
  Unset,
  Unknown,
}

pub(super) type Evaluation<T> = anyhow::Result<Evaluated<T>>;

// Runtime evaluation builds the returned Expression directly; constant
// analysis borrows strings and stops at host-dependent leaves.
pub(super) trait Evaluator<'a> {
  type Value;

  fn eval(&mut self, expression: &'a Expression) -> Evaluation<Self::Value>;
  fn leaf(&mut self, expression: &'a Expression) -> Evaluation<Self::Value>;
  fn boolean(value: bool) -> Self::Value;
  fn number(value: f64) -> Self::Value;
  fn list(values: Vec<Self::Value>) -> Self::Value;
  fn into_boolean(value: Self::Value) -> anyhow::Result<bool>;
  fn into_number(value: Self::Value) -> anyhow::Result<f64>;
  fn empty_extrema(name: &str) -> Evaluation<Self::Value>;

  fn list_fits(&self, _len: usize) -> bool {
    true
  }
}

macro_rules! value {
  ($evaluation:expr) => {
    match $evaluation? {
      Evaluated::Value(value) => value,
      Evaluated::Unset => return Ok(Evaluated::Unset),
      Evaluated::Unknown => return Ok(Evaluated::Unknown),
    }
  };
}

pub(super) fn evaluate<'a, E: Evaluator<'a>>(
  expression: &'a Expression,
  evaluator: &mut E,
) -> Evaluation<E::Value> {
  use Expression::*;

  macro_rules! number {
    ($operand:expr) => {
      E::into_number(value!(evaluator.eval($operand)))?
    };
  }

  let result = match expression {
    List(items) => {
      if !evaluator.list_fits(items.len()) {
        return Ok(Evaluated::Unknown);
      }
      let mut result = Vec::with_capacity(items.len());
      for item in items {
        result.push(value!(evaluator.eval(item)));
      }
      E::list(result)
    },
    Plus { a, b } => E::number(finite(number!(a) + number!(b))?),
    Minus { a, b } => E::number(finite(number!(a) - number!(b))?),
    Multiply { a, b } => E::number(finite(number!(a) * number!(b))?),
    Divide { a, b } => E::number(finite(number!(a) / number!(b))?),
    Power { a, b } => E::number(finite(number!(a).powf(number!(b)))?),
    LessThan { a, b } => E::boolean(number!(a) < number!(b)),
    MoreThan { a, b } => E::boolean(number!(a) > number!(b)),
    Equal { a, b, leeway } => {
      let a = number!(a);
      let b = number!(b);
      let leeway = number!(leeway);
      E::boolean(a - leeway < b && b < a + leeway)
    },
    Minimum { numbers } | Maximum { numbers } => {
      let minimum = matches!(expression, Minimum { .. });
      let Some((first, rest)) = numbers.split_first() else {
        return E::empty_extrema(if minimum { "minimum" } else { "maximum" });
      };
      let mut extreme = number!(first);
      for operand in rest {
        let number = number!(operand);
        let order = number.total_cmp(&extreme);
        let replaces_extreme = if minimum {
          order.is_lt()
        } else {
          order.is_gt()
        };
        if replaces_extreme {
          extreme = number;
        }
      }
      E::number(extreme)
    },
    IsUnset { a } => {
      match evaluator.eval(a)? {
        Evaluated::Value(_) => E::boolean(false),
        Evaluated::Unset => E::boolean(true),
        Evaluated::Unknown => return Ok(Evaluated::Unknown),
      }
    },
    IfElse {
      condition,
      consequence,
      alternative,
    } => {
      return if E::into_boolean(value!(evaluator.eval(condition)))? {
        evaluator.eval(consequence)
      } else if let Some(alternative) = alternative {
        evaluator.eval(alternative)
      } else {
        Ok(Evaluated::Unset)
      };
    },
    And { a, b } => return logic(evaluator, [a.as_ref(), b.as_ref()], false),
    Or { a, b } => return logic(evaluator, [a.as_ref(), b.as_ref()], true),
    All { all } => return logic(evaluator, all, false),
    Any { any } => return logic(evaluator, any, true),
    Not { not } => E::boolean(!E::into_boolean(value!(evaluator.eval(not)))?),
    _ => return evaluator.leaf(expression),
  };
  Ok(Evaluated::Value(result))
}

fn finite(result: f64) -> anyhow::Result<f64> {
  if !result.is_finite() {
    bail!("arithmetic expression produced a non-finite number");
  }
  Ok(result)
}

fn logic<'a, E: Evaluator<'a>>(
  evaluator: &mut E,
  operands: impl IntoIterator<Item = &'a Expression>,
  decisive: bool,
) -> Evaluation<E::Value> {
  for operand in operands {
    if E::into_boolean(value!(evaluator.eval(operand)))? == decisive {
      return Ok(Evaluated::Value(E::boolean(decisive)));
    }
  }
  Ok(Evaluated::Value(E::boolean(!decisive)))
}

impl<'a> Evaluator<'a> for &EvalState<'_, '_> {
  type Value = Expression;

  fn eval(&mut self, expression: &'a Expression) -> Evaluation<Expression> {
    log::trace!("evaluating expression: {expression:?}");
    evaluate(expression, self)
  }

  fn leaf(&mut self, expression: &'a Expression) -> Evaluation<Expression> {
    Ok(match expression.eval_leaf(self)? {
      Some(value) => Evaluated::Value(value),
      None => Evaluated::Unset,
    })
  }

  fn boolean(value: bool) -> Expression {
    Expression::Boolean(value)
  }

  fn number(value: f64) -> Expression {
    Expression::Number(value)
  }

  fn list(values: Vec<Expression>) -> Expression {
    Expression::List(values)
  }

  fn into_boolean(value: Expression) -> anyhow::Result<bool> {
    value.try_into_boolean()
  }

  fn into_number(value: Expression) -> anyhow::Result<f64> {
    value.try_into_number()
  }

  fn empty_extrema(name: &str) -> Evaluation<Expression> {
    bail!("{name} must be given at least 1 expression");
  }
}

impl Expression {
  pub fn eval(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<Self>> {
    let mut evaluator = state;
    match Evaluator::eval(&mut evaluator, self)? {
      Evaluated::Value(value) => Ok(Some(value)),
      Evaluated::Unset => Ok(None),
      Evaluated::Unknown => unreachable!("runtime evaluation is concrete"),
    }
  }

  fn eval_leaf(
    &self,
    state: &EvalState<'_, '_>,
  ) -> anyhow::Result<Option<Self>> {
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

      _ => unreachable!("pure operators are evaluated separately"),
    }))
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

  let (total, count) = CpuLog::recent(state.cpu_log, state.now, duration)
    .fold((0.0, 0usize), |(total, count), log| {
      (total + value(log), count + 1)
    });

  // Each sample measures an interval, so one sample suffices for a short
  // window.
  Ok((count > 0).then(|| total / count as f64))
}
