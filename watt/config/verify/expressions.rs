use std::time::Duration;

use humantime::parse_duration;

use super::{
  Diagnostic,
  Expression,
  Rule,
  Severity,
  constants,
  expression_type,
};
use crate::system::{
  HISTORY_SAMPLE_LIMIT,
  MAX_POLL_INTERVAL_SECONDS,
};

pub(super) fn verify_rule_expressions(
  rule: &Rule,
  diagnostics: &mut Vec<Diagnostic>,
) {
  let mut pending = Vec::new();
  for (field, expression) in rule_expressions(rule) {
    pending.push(expression);
    while let Some(expression) = pending.pop() {
      let constant = constants::analyze(expression);
      let visit_children = matches!(constant, Ok(constants::Constant::Unknown));
      if let Some((severity, message)) = duration_diagnostic(expression)
        .or_else(|| {
          visit_children
            .then(|| operator_error(expression))
            .flatten()
            .map(|message| (Severity::Error, message))
        })
        .or_else(|| {
          constant
            .err()
            .map(|error| (Severity::Error, error.to_string()))
        })
        .or_else(|| empty_collection_diagnostic(expression))
      {
        diagnostics.push(Diagnostic {
          severity,
          rule: rule.name.clone(),
          priority: rule.priority,
          field: field.into(),
          message,
        });
      }
      if visit_children {
        push_children(expression, &mut pending);
      }
    }
  }
}

fn empty_collection_diagnostic(
  expression: &Expression,
) -> Option<(Severity, String)> {
  match expression {
    Expression::Minimum { numbers } | Expression::Maximum { numbers }
      if numbers.is_empty() =>
    {
      Some((
        Severity::Error,
        "minimum/maximum requires at least one expression".into(),
      ))
    },
    Expression::FirstAvailableGovernor { values }
    | Expression::FirstAvailableEnergyPerformancePreference { values }
    | Expression::FirstAvailableEnergyPerfBias { values }
    | Expression::FirstAvailablePlatformProfile { values }
      if values.is_empty() =>
    {
      Some((
        Severity::Warning,
        "empty fallback list never selects a value".into(),
      ))
    },
    _ => None,
  }
}

fn operator_error(expression: &Expression) -> Option<String> {
  use Expression::*;
  match expression {
    Plus { a, b }
    | Minus { a, b }
    | Multiply { a, b }
    | Divide { a, b }
    | Power { a, b }
    | LessThan { a, b }
    | MoreThan { a, b } => {
      operand_type_error([a.as_ref(), b.as_ref()], "number")
    },
    Equal { a, b, leeway } => {
      operand_type_error([a.as_ref(), b.as_ref(), leeway.as_ref()], "number")
    },
    And { a, b } | Or { a, b } => {
      operand_type_error([a.as_ref(), b.as_ref()], "boolean")
    },
    All { all: values } | Any { any: values } => {
      operand_type_error(values, "boolean")
    },
    Not { not: value }
    | IfElse {
      condition: value, ..
    } => operand_type_error([value.as_ref()], "boolean"),
    Minimum { numbers } | Maximum { numbers } => {
      operand_type_error(numbers, "number")
    },
    IsGovernorAvailable { value }
    | IsEnergyPerformancePreferenceAvailable { value }
    | IsEnergyPerfBiasAvailable { value }
    | IsPlatformProfileAvailable { value }
    | IsChassisType { value }
    | IsDriverLoaded { value }
    | IsBatteryAvailable { value } => {
      operand_type_error([value.as_ref()], "string")
    },
    FirstAvailableGovernor { values }
    | FirstAvailableEnergyPerformancePreference { values }
    | FirstAvailableEnergyPerfBias { values }
    | FirstAvailablePlatformProfile { values } => {
      operand_type_error(values, "string")
    },
    _ => None,
  }
}

fn operand_type_error<'a>(
  operands: impl IntoIterator<Item = &'a Expression>,
  expected: &str,
) -> Option<String> {
  operands.into_iter().find_map(|operand| {
    expression_type(operand)
      .filter(|actual| *actual != expected)
      .map(|actual| format!("expected {expected} operand, got {actual}"))
  })
}

fn duration_diagnostic(expression: &Expression) -> Option<(Severity, String)> {
  let (name, duration) = match expression {
    Expression::CpuUsageSince { duration } => ("cpu-usage-since", duration),
    Expression::LoadAverageSince { duration } => {
      ("load-average-since", duration)
    },
    _ => return None,
  };
  duration_value_diagnostic(duration)
    .map(|(severity, message)| (severity, format!("{name}: {message}")))
}

fn duration_value_diagnostic(
  duration: &Expression,
) -> Option<(Severity, String)> {
  match constants::analyze(duration).ok()? {
    constants::Constant::String(value) => {
      match parse_duration(value) {
        Ok(duration) => {
          sampling_warning(duration).map(|message| (Severity::Warning, message))
        },
        Err(error) => Some((Severity::Error, error.to_string())),
      }
    },
    constants::Constant::Unknown => {
      expression_type(duration)
        .filter(|kind| *kind != "string")
        .map(|_| (Severity::Error, "duration must be a string".into()))
    },
    constants::Constant::Unset => None,
    _ => Some((Severity::Error, "duration must be a string".into())),
  }
}

fn sampling_warning(duration: Duration) -> Option<String> {
  if duration.is_zero() {
    return Some("zero-length window never includes a sample".into());
  }
  let retained_seconds =
    (HISTORY_SAMPLE_LIMIT as u64 - 1) * MAX_POLL_INTERVAL_SECONDS;
  if duration > Duration::from_secs(retained_seconds) {
    return Some(format!(
      "history retains {HISTORY_SAMPLE_LIMIT} samples across at most {} \
       polling intervals. At the slowest normal cadence of \
       {MAX_POLL_INTERVAL_SECONDS} seconds, that covers {retained_seconds} \
       seconds. This window is longer; its average uses only retained samples",
      HISTORY_SAMPLE_LIMIT - 1
    ));
  }
  None
}

pub(super) fn rule_expressions(
  rule: &Rule,
) -> impl Iterator<Item = (&'static str, &Expression)> {
  [
    ("if", Some(&rule.condition)),
    ("cpu.for", rule.cpu.for_.as_ref()),
    ("cpu.governor", rule.cpu.governor.as_ref()),
    (
      "cpu.energy-performance-preference",
      rule.cpu.energy_performance_preference.as_ref(),
    ),
    ("cpu.energy-perf-bias", rule.cpu.energy_perf_bias.as_ref()),
    (
      "cpu.frequency-mhz-minimum",
      rule.cpu.frequency_mhz_minimum.as_ref(),
    ),
    (
      "cpu.frequency-mhz-maximum",
      rule.cpu.frequency_mhz_maximum.as_ref(),
    ),
    ("cpu.turbo", rule.cpu.turbo.as_ref()),
    (
      "cpu.pstate-min-performance-percent",
      rule.cpu.pstate_min_performance_percent.as_ref(),
    ),
    (
      "cpu.pstate-max-performance-percent",
      rule.cpu.pstate_max_performance_percent.as_ref(),
    ),
    ("cpu.dma-latency-us", rule.cpu.dma_latency_us.as_ref()),
    (
      "cpu.pm-qos-resume-latency-us",
      rule.cpu.pm_qos_resume_latency_us.as_ref(),
    ),
    ("uncore.for", rule.uncore.for_.as_ref()),
    (
      "uncore.frequency-khz-minimum",
      rule.uncore.frequency_khz_minimum.as_ref(),
    ),
    (
      "uncore.frequency-khz-maximum",
      rule.uncore.frequency_khz_maximum.as_ref(),
    ),
    ("vm.dirty-bytes", rule.vm.dirty_bytes.as_ref()),
    (
      "vm.dirty-background-bytes",
      rule.vm.dirty_background_bytes.as_ref(),
    ),
    ("vm.dirty-ratio", rule.vm.dirty_ratio.as_ref()),
    (
      "vm.dirty-background-ratio",
      rule.vm.dirty_background_ratio.as_ref(),
    ),
    (
      "vm.transparent-hugepages",
      rule.vm.transparent_hugepages.as_ref(),
    ),
    (
      "vm.transparent-hugepage-defrag",
      rule.vm.transparent_hugepage_defrag.as_ref(),
    ),
    ("disk.for", rule.disk.for_.as_ref()),
    ("disk.scheduler", rule.disk.scheduler.as_ref()),
    ("disk.readahead-kib", rule.disk.readahead_kib.as_ref()),
    ("disk.apm", rule.disk.apm.as_ref()),
    ("disk.spindown", rule.disk.spindown.as_ref()),
    ("disk.alpm", rule.disk.alpm.as_ref()),
    ("usb.for", rule.usb.for_.as_ref()),
    ("usb.autosuspend", rule.usb.autosuspend.as_ref()),
    (
      "usb.autosuspend-delay-seconds",
      rule.usb.autosuspend_delay_seconds.as_ref(),
    ),
    ("audio.timeout-seconds", rule.audio.timeout_seconds.as_ref()),
    (
      "audio.reset-controller",
      rule.audio.reset_controller.as_ref(),
    ),
    ("gpu.for", rule.gpu.for_.as_ref()),
    (
      "gpu.panel-power-savings",
      rule.gpu.panel_power_savings.as_ref(),
    ),
    ("gpu.radeon-powersave", rule.gpu.radeon_powersave.as_ref()),
    ("power.for", rule.power.for_.as_ref()),
    (
      "power.charge-threshold-start",
      rule.power.charge_threshold_start.as_ref(),
    ),
    (
      "power.charge-threshold-end",
      rule.power.charge_threshold_end.as_ref(),
    ),
    (
      "power.platform-profile",
      rule.power.platform_profile.as_ref(),
    ),
  ]
  .into_iter()
  .filter_map(|(field, expression)| {
    expression.map(|expression| (field, expression))
  })
}

fn push_children<'a>(
  expression: &'a Expression,
  pending: &mut Vec<&'a Expression>,
) {
  use Expression::*;
  match expression {
    IsGovernorAvailable { value }
    | IsEnergyPerformancePreferenceAvailable { value }
    | IsEnergyPerfBiasAvailable { value }
    | IsPlatformProfileAvailable { value }
    | IsChassisType { value }
    | IsDriverLoaded { value }
    | IsBatteryAvailable { value }
    | CpuUsageSince { duration: value }
    | LoadAverageSince { duration: value }
    | IsUnset { a: value }
    | Not { not: value } => pending.push(value),
    FirstAvailableGovernor { values }
    | FirstAvailableEnergyPerformancePreference { values }
    | FirstAvailableEnergyPerfBias { values }
    | FirstAvailablePlatformProfile { values }
    | List(values)
    | Minimum { numbers: values }
    | Maximum { numbers: values }
    | All { all: values }
    | Any { any: values } => pending.extend(values),
    Plus { a, b }
    | Minus { a, b }
    | Multiply { a, b }
    | Power { a, b }
    | Divide { a, b }
    | LessThan { a, b }
    | MoreThan { a, b }
    | And { a, b }
    | Or { a, b } => pending.extend([a.as_ref(), b.as_ref()]),
    Equal { a, b, leeway } => {
      pending.extend([a.as_ref(), b.as_ref(), leeway.as_ref()])
    },
    IfElse {
      condition,
      consequence,
      alternative,
    } => {
      push_conditional_children(
        condition,
        consequence,
        alternative.as_deref(),
        pending,
      );
    },
    FrequencyAvailable
    | TurboAvailable
    | CpuUsage
    | CpuUsageVolatility
    | CpuTemperature
    | CpuTemperatureVolatility
    | CpuIdleSeconds
    | CpuFrequencyMaximum
    | CpuFrequencyMinimum
    | CpuScalingMaximum
    | CpuCoreCount
    | LidClosed
    | VirtualMachine
    | HourOfDay
    | PowerSupplyCharge
    | PowerSupplyDischargeRate
    | BatteryCycles
    | BatteryHealth
    | BatteryCyclesFor { .. }
    | BatteryHealthFor { .. }
    | Discharging
    | PowerProfilePreference
    | Boolean(_)
    | Number(_)
    | String(_) => {},
  }
}

fn push_conditional_children<'a>(
  condition: &'a Expression,
  consequence: &'a Expression,
  alternative: Option<&'a Expression>,
  pending: &mut Vec<&'a Expression>,
) {
  pending.push(condition);
  match constants::boolean(condition) {
    Some(true) => pending.push(consequence),
    Some(false) => pending.extend(alternative),
    None => {
      pending.push(consequence);
      pending.extend(alternative);
    },
  }
}

#[cfg(test)]
mod tests {
  use super::super::{
    Severity,
    tests::{
      load_error,
      load_rule,
    },
  };

  #[test]
  fn loading_distinguishes_empty_extrema_from_empty_fallbacks() {
    for operation in ["minimum", "maximum"] {
      let error =
        load_error(&format!("cpu.frequency-mhz-minimum={{{operation}=[]}}"));
      assert!(
        error.contains("cpu.frequency-mhz-minimum")
          && error.contains("at least one"),
        "{error}"
      );
    }
    for (field, operation) in [
      ("cpu.governor", "first-available-governor"),
      (
        "cpu.energy-performance-preference",
        "first-available-energy-performance-preference",
      ),
      ("cpu.energy-perf-bias", "first-available-energy-perf-bias"),
      ("power.platform-profile", "first-available-platform-profile"),
    ] {
      let diagnostics =
        load_rule(&format!("if={{all=[]}}\n{field}={{{operation}=[]}}"))
          .unwrap()
          .verify_static();
      assert_eq!(diagnostics.len(), 2);
      assert!(
        diagnostics
          .iter()
          .all(|diagnostic| diagnostic.severity == Severity::Warning)
      );
      assert!(
        diagnostics
          .iter()
          .any(|diagnostic| diagnostic.field == "if")
      );
      assert!(
        diagnostics
          .iter()
          .any(|diagnostic| diagnostic.field == field)
      );
    }
  }

  #[test]
  fn loading_rejects_invalid_constant_arithmetic() {
    for (expression, message) in [
      ("{value=1,divide=0}", "non-finite"),
      ("{value=0,divide=0}", "non-finite"),
      ("{value=1e308,multiply=1e308}", "non-finite"),
      ("{value=-1,power=0.5}", "non-finite"),
      ("{value=5,divide={value=2,minus=2}}", "non-finite"),
      ("{value='1',plus=1}", "expected number operand"),
      ("{value=1,plus=false}", "expected number operand"),
      (
        "{value='$cpu-temperature',plus=[]}",
        "expected number operand",
      ),
    ] {
      let error =
        load_error(&format!("cpu.frequency-mhz-minimum={expression}"));
      assert!(error.contains("cpu.frequency-mhz-minimum"), "{error}");
      assert!(error.contains(message), "{error}");
    }
    for expression in [
      "{value=2,plus={value=3,multiply=4}}",
      "{value='$cpu-temperature',divide=2}",
    ] {
      load_rule(&format!("cpu.frequency-mhz-minimum={expression}")).unwrap();
    }
  }

  #[test]
  fn loading_checks_operator_operand_types() {
    for expression in [
      "{value='1',is-less-than=2}",
      "{value=1,is-equal=2,leeway=false}",
      "{all=[true,1]}",
      "{not=[]}",
      "{if=1,then=true,else=false}",
      "{value={minimum=[1,false]},is-more-than=0}",
      "{is-driver-loaded=1}",
      "{is-governor-available={first-available-governor=[false]}}",
    ] {
      let error = load_error(&format!("if={expression}"));
      assert!(
        error.contains("operand") && error.contains("expected"),
        "{error}"
      );
    }
    load_rule(
      r#"if={all=['?discharging',{value={maximum=[1,2]},is-more-than=0}]}
cpu.governor={first-available-governor=['powersave',{if='?discharging',then='schedutil'}]}"#,
    )
    .unwrap();
  }

  #[test]
  fn loading_rejects_nested_invalid_duration_expressions() {
    for (field, expression) in [
      (
        "if",
        "{all=[true,{value={cpu-usage-since='yesterday'},is-more-than=1}]}",
      ),
      (
        "cpu.frequency-mhz-minimum",
        "{if='?discharging',then=100,else={load-average-since=3}}",
      ),
      ("usb.for", "[{load-average-since=[]}]"),
      (
        "audio.timeout-seconds",
        "{cpu-usage-since={value=1,plus=1}}",
      ),
      (
        "power.charge-threshold-end",
        "{maximum=[80,{cpu-usage-since=false}]}",
      ),
    ] {
      let error = load_error(&format!("{field}={expression}"));
      assert!(error.contains(field), "{error}");
      assert!(error.contains("since"), "{error}");
    }
    load_rule(
      r#"if={value={cpu-usage-since='1s'},is-more-than=5}
cpu.frequency-mhz-minimum={load-average-since={if='?discharging',then='1s',else='5m'}}"#,
    )
    .unwrap();
  }
}
