use std::{
  cmp::Reverse,
  collections::HashMap,
  fmt,
  time::Duration,
};

use anyhow::{
  anyhow,
  bail,
  ensure,
};
use serde::Serialize;

use super::{
  CpusDelta,
  DaemonConfig,
  Expression,
  Rule,
};
use crate::{
  cpu::frequency::Frequency,
  vm::{
    validate_hugepage_defrag,
    validate_hugepage_mode,
  },
};

mod constants;
mod expressions;
mod hardware;

/// Evaluates a constant string expression as a duration without reading
/// telemetry.
pub(crate) fn constant_duration(expression: &Expression) -> Option<Duration> {
  let constants::Constant::String(value) =
    constants::analyze(expression).ok()?
  else {
    return None;
  };
  humantime::parse_duration(value).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
  Error,
  Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
  pub severity: Severity,
  pub rule:     String,
  pub priority: u16,
  pub field:    String,
  pub message:  String,
}

impl Diagnostic {
  fn new(
    severity: Severity,
    rule: &Rule,
    field: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self {
      severity,
      rule: rule.name.clone(),
      priority: rule.priority,
      field: field.into(),
      message: message.into(),
    }
  }

  fn error(
    rule: &Rule,
    field: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self::new(Severity::Error, rule, field, message)
  }

  fn warning(
    rule: &Rule,
    field: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self::new(Severity::Warning, rule, field, message)
  }
}

impl fmt::Display for Diagnostic {
  fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      output,
      "rule {:?} (priority {}), {}: {}",
      self.rule, self.priority, self.field, self.message
    )
  }
}

impl DaemonConfig {
  /// Checks rule names, priorities, expression types, and constant action
  /// bounds.
  pub fn verify_static(&self) -> Vec<Diagnostic> {
    let mut diagnostics = self.verify_rule_identities();
    for rule in &self.rules {
      expressions::verify_rule_expressions(rule, &mut diagnostics);
      verify_condition(rule, &mut diagnostics);
      verify_selections(rule, &mut diagnostics);
      verify_action_types(rule, &mut diagnostics);
      verify_vm_numbers(rule, &mut diagnostics);
      verify_vm_conflicts(rule, &mut diagnostics);
      verify_vm_modes(rule, &mut diagnostics);
      verify_device_numbers(rule, &mut diagnostics);
      verify_charge_threshold_values(rule, &mut diagnostics);
      if let Some(error) =
        literal_epp_error(rule.cpu.energy_performance_preference.as_ref())
      {
        diagnostics.push(Diagnostic::error(
          rule,
          "cpu.energy-performance-preference",
          error,
        ));
      }
      verify_bounds(
        rule,
        [
          ("cpu.frequency-mhz-minimum", &rule.cpu.frequency_mhz_minimum),
          ("cpu.frequency-mhz-maximum", &rule.cpu.frequency_mhz_maximum),
        ],
        constant_frequency,
        &mut diagnostics,
      );
      verify_bounds(
        rule,
        [
          (
            "cpu.pstate-min-performance-percent",
            &rule.cpu.pstate_min_performance_percent,
          ),
          (
            "cpu.pstate-max-performance-percent",
            &rule.cpu.pstate_max_performance_percent,
          ),
        ],
        constant_percentage,
        &mut diagnostics,
      );
      verify_bounds(
        rule,
        [
          (
            "uncore.frequency-khz-minimum",
            &rule.uncore.frequency_khz_minimum,
          ),
          (
            "uncore.frequency-khz-maximum",
            &rule.uncore.frequency_khz_maximum,
          ),
        ],
        constant_unsigned_integer,
        &mut diagnostics,
      );
    }
    verify_shadowed_conditions(&self.rules, &mut diagnostics);
    diagnostics
  }

  pub fn verify_rule_identities(&self) -> Vec<Diagnostic> {
    let mut names = HashMap::new();
    let mut priorities = HashMap::new();
    let mut diagnostics = Vec::new();
    for rule in &self.rules {
      if let Some(priority) = names.insert(&rule.name, rule.priority) {
        diagnostics.push(Diagnostic::error(
          rule,
          "name",
          format!("name is also used at priority {priority}"),
        ));
      }
      if let Some(name) = priorities.insert(rule.priority, &rule.name) {
        diagnostics.push(Diagnostic::error(
          rule,
          "priority",
          format!("priority is also used by rule {name:?}"),
        ));
      }
    }
    diagnostics
  }
}

fn verify_vm_modes(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  for (field, expression, validate) in [
    (
      "vm.transparent-hugepages",
      &rule.vm.transparent_hugepages,
      validate_hugepage_mode as fn(&str) -> anyhow::Result<()>,
    ),
    (
      "vm.transparent-hugepage-defrag",
      &rule.vm.transparent_hugepage_defrag,
      validate_hugepage_defrag,
    ),
  ] {
    if let Some(expression) = expression
      && let Ok(constants::Constant::String(value)) =
        constants::analyze(expression)
      && let Err(error) = validate(value)
    {
      diagnostics.push(Diagnostic::error(rule, field, error.to_string()));
    }
  }
}

fn verify_device_numbers(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  let resume_latency =
    rule
      .cpu
      .pm_qos_resume_latency_us
      .as_ref()
      .filter(|expression| {
        !matches!(
          constants::analyze(expression),
          Ok(constants::Constant::String("n/a"))
        )
      });
  for (field, expression, maximum) in [
    (
      "cpu.dma-latency-us",
      rule.cpu.dma_latency_us.as_ref(),
      i32::MAX as u64,
    ),
    ("cpu.pm-qos-resume-latency-us", resume_latency, u64::MAX),
    (
      "disk.readahead-kib",
      rule.disk.readahead_kib.as_ref(),
      u64::MAX,
    ),
    ("disk.apm", rule.disk.apm.as_ref(), u64::from(u8::MAX)),
    (
      "disk.spindown",
      rule.disk.spindown.as_ref(),
      u64::from(u8::MAX),
    ),
    (
      "audio.timeout-seconds",
      rule.audio.timeout_seconds.as_ref(),
      u64::MAX,
    ),
    (
      "usb.autosuspend-delay-seconds",
      rule.usb.autosuspend_delay_seconds.as_ref(),
      u64::MAX / 1000,
    ),
    (
      "gpu.panel-power-savings",
      rule.gpu.panel_power_savings.as_ref(),
      4,
    ),
  ] {
    let result = constant_unsigned_integer(expression).and_then(|value| {
      if let Some(value) = value {
        ensure!(value <= maximum, "value {value} exceeds maximum {maximum}");
      }
      Ok(())
    });
    if let Err(error) = result {
      diagnostics.push(Diagnostic::error(rule, field, error.to_string()));
    }
  }
}

fn verify_vm_conflicts(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  for (bytes_field, bytes, ratio_field, ratio) in [
    (
      "vm.dirty-bytes",
      &rule.vm.dirty_bytes,
      "vm.dirty-ratio",
      &rule.vm.dirty_ratio,
    ),
    (
      "vm.dirty-background-bytes",
      &rule.vm.dirty_background_bytes,
      "vm.dirty-background-ratio",
      &rule.vm.dirty_background_ratio,
    ),
  ] {
    if let (Ok(Some(_)), Ok(Some(_))) = (
      constant_unsigned_integer(bytes.as_ref()),
      constant_percentage(ratio.as_ref()),
    ) {
      diagnostics.push(Diagnostic::error(
        rule,
        bytes_field,
        format!("conflicts with `{ratio_field}`"),
      ));
    }
  }
}

fn verify_vm_numbers(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  for (field, error) in [
    (
      "vm.dirty-bytes",
      constant_unsigned_integer(rule.vm.dirty_bytes.as_ref()).err(),
    ),
    (
      "vm.dirty-background-bytes",
      constant_unsigned_integer(rule.vm.dirty_background_bytes.as_ref()).err(),
    ),
    (
      "vm.dirty-ratio",
      constant_percentage(rule.vm.dirty_ratio.as_ref()).err(),
    ),
    (
      "vm.dirty-background-ratio",
      constant_percentage(rule.vm.dirty_background_ratio.as_ref()).err(),
    ),
  ] {
    if let Some(error) = error {
      diagnostics.push(Diagnostic::error(rule, field, error.to_string()));
    }
  }
}

fn verify_action_types(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  for (field, expression, expected) in [
    ("cpu.governor", &rule.cpu.governor, "string"),
    ("cpu.energy-perf-bias", &rule.cpu.energy_perf_bias, "string"),
    ("cpu.turbo", &rule.cpu.turbo, "boolean"),
    (
      "vm.transparent-hugepages",
      &rule.vm.transparent_hugepages,
      "string",
    ),
    (
      "vm.transparent-hugepage-defrag",
      &rule.vm.transparent_hugepage_defrag,
      "string",
    ),
    ("disk.scheduler", &rule.disk.scheduler, "string"),
    ("disk.alpm", &rule.disk.alpm, "string"),
    ("usb.autosuspend", &rule.usb.autosuspend, "boolean"),
    (
      "audio.reset-controller",
      &rule.audio.reset_controller,
      "boolean",
    ),
    ("gpu.radeon-powersave", &rule.gpu.radeon_powersave, "string"),
    (
      "power.platform-profile",
      &rule.power.platform_profile,
      "string",
    ),
  ] {
    if let Some(actual) = expression.as_ref().and_then(expression_type)
      && actual != expected
    {
      diagnostics.push(Diagnostic::error(
        rule,
        field,
        format!("expected {expected}, got {actual}"),
      ));
    }
  }
}

fn expression_type(expression: &Expression) -> Option<&'static str> {
  expression_type_bounded(expression, 0)
}

fn expression_type_bounded(
  expression: &Expression,
  depth: usize,
) -> Option<&'static str> {
  if depth >= 64 {
    return None;
  }
  if let Ok(value) = constants::analyze(expression)
    && let Some(kind) = value.kind()
  {
    return Some(kind);
  }
  use Expression::*;
  match expression {
    Boolean(_)
    | IsGovernorAvailable { .. }
    | IsEnergyPerformancePreferenceAvailable { .. }
    | IsEnergyPerfBiasAvailable { .. }
    | IsPlatformProfileAvailable { .. }
    | IsChassisType { .. }
    | IsDriverLoaded { .. }
    | IsBatteryAvailable { .. }
    | FrequencyAvailable
    | TurboAvailable
    | LidClosed
    | VirtualMachine
    | Discharging
    | LessThan { .. }
    | MoreThan { .. }
    | Equal { .. }
    | IsUnset { .. }
    | And { .. }
    | Or { .. }
    | All { .. }
    | Any { .. }
    | Not { .. } => Some("boolean"),
    Number(_)
    | CpuUsage
    | CpuUsageVolatility
    | CpuUsageSince { .. }
    | LoadAverageSince { .. }
    | CpuTemperature
    | CpuTemperatureVolatility
    | CpuIdleSeconds
    | CpuFrequencyMaximum
    | CpuFrequencyMinimum
    | CpuScalingMaximum
    | CpuCoreCount
    | HourOfDay
    | PowerSupplyCharge
    | PowerSupplyDischargeRate
    | BatteryCycles
    | BatteryHealth
    | BatteryCyclesFor { .. }
    | BatteryHealthFor { .. }
    | Plus { .. }
    | Minus { .. }
    | Multiply { .. }
    | Power { .. }
    | Divide { .. }
    | Minimum { .. }
    | Maximum { .. } => Some("number"),
    String(_)
    | FirstAvailableGovernor { .. }
    | FirstAvailableEnergyPerformancePreference { .. }
    | FirstAvailableEnergyPerfBias { .. }
    | FirstAvailablePlatformProfile { .. }
    | PowerProfilePreference => Some("string"),
    List(_) => Some("list"),
    IfElse {
      condition,
      consequence,
      alternative,
    } => {
      let depth = depth + 1;
      let consequence = expression_type_bounded(consequence, depth);
      let alternative = alternative
        .as_deref()
        .map(|value| expression_type_bounded(value, depth));
      match constants::boolean(condition) {
        Some(true) => consequence,
        Some(false) => alternative.flatten(),
        None => {
          consequence
            .filter(|kind| alternative.is_none_or(|other| other == Some(*kind)))
        },
      }
    },
  }
}

fn verify_selections(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  for (field, expression, subject, expected) in [
    ("cpu.for", &rule.cpu.for_, "CPU", "number"),
    ("uncore.for", &rule.uncore.for_, "device", "string"),
    ("disk.for", &rule.disk.for_, "device", "string"),
    ("usb.for", &rule.usb.for_, "device", "string"),
    ("gpu.for", &rule.gpu.for_, "device", "string"),
    ("power.for", &rule.power.for_, "device", "string"),
  ] {
    let Some(expression) = expression else {
      continue;
    };
    let items: Vec<_> = match (constants::analyze(expression), expression) {
      (Ok(constants::Constant::List(values)), _) => {
        values
          .iter()
          .map(|value| {
            let number = match value {
              constants::Constant::Number(number) => Some(*number),
              _ => None,
            };
            (value.kind(), number)
          })
          .collect()
      },
      (_, Expression::List(values)) => {
        values
          .iter()
          .map(|value| (expression_type(value), folded_number(value)))
          .collect()
      },
      _ => {
        if expression_type(expression).is_some_and(|kind| kind != "list") {
          diagnostics.push(Diagnostic::error(
            rule,
            field,
            format!("{subject} selection must be a list"),
          ));
        }
        continue;
      },
    };
    for (index, (kind, number)) in items.into_iter().enumerate() {
      let result = if kind.is_some_and(|kind| kind != expected) {
        Err(anyhow!("{subject} selection item must be a {expected}"))
      } else if field == "cpu.for"
        && let Some(number) = number
      {
        CpusDelta::cpu_number(Expression::Number(number)).map(|_| ())
      } else {
        Ok(())
      };
      if let Err(error) = result {
        diagnostics.push(Diagnostic::error(
          rule,
          format!("{field}[{index}]"),
          error.to_string(),
        ));
      }
    }
  }
}

fn verify_condition(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  let diagnostic = match constants::boolean(&rule.condition) {
    Some(false) => {
      Diagnostic::warning(
        rule,
        "if",
        "rule never matches because its condition is false",
      )
    },
    Some(true) if rule.condition != Expression::Boolean(true) => {
      Diagnostic::warning(rule, "if", "rule condition always evaluates to true")
    },
    _ if expression_type(&rule.condition)
      .is_some_and(|kind| kind != "boolean") =>
    {
      Diagnostic::error(rule, "if", "condition must be a boolean")
    },
    _ => return,
  };
  diagnostics.push(diagnostic);
}

fn verify_charge_threshold_values(
  rule: &Rule,
  diagnostics: &mut Vec<Diagnostic>,
) {
  for (field, expression) in [
    (
      "power.charge-threshold-start",
      &rule.power.charge_threshold_start,
    ),
    (
      "power.charge-threshold-end",
      &rule.power.charge_threshold_end,
    ),
  ] {
    if let Err(error) = constant_percentage(expression.as_ref()) {
      diagnostics.push(Diagnostic::error(rule, field, error.to_string()));
    }
  }
}

fn literal_epp_error(expression: Option<&Expression>) -> Option<String> {
  let expression = expression?;
  if expression_type(expression)
    .is_some_and(|kind| kind != "string" && kind != "number")
  {
    return Some("EPP must be a string or an integer from 0 to 255".into());
  }
  let invalid = match constants::analyze(expression).ok()? {
    constants::Constant::String(value) => {
      value.parse::<f64>().is_ok() && value.parse::<u8>().is_err()
    },
    constants::Constant::Number(value) => {
      !super::unsigned_integer(value)
        .is_ok_and(|value| value <= u64::from(u8::MAX))
    },
    _ => false,
  };
  invalid.then(|| "numeric EPP must be an integer from 0 to 255".into())
}

fn verify_bounds<T: Copy + PartialOrd + fmt::Display>(
  rule: &Rule,
  fields: [(&str, &Option<Expression>); 2],
  convert: fn(Option<&Expression>) -> anyhow::Result<Option<T>>,
  diagnostics: &mut Vec<Diagnostic>,
) {
  let mut bounds = [None, None];
  for (bound, (field, expression)) in bounds.iter_mut().zip(fields) {
    match convert(expression.as_ref()) {
      Ok(value) => *bound = value,
      Err(error) => {
        diagnostics.push(Diagnostic::error(rule, field, error.to_string()))
      },
    }
  }
  if let [Some(minimum), Some(maximum)] = bounds
    && minimum > maximum
  {
    diagnostics.push(Diagnostic::error(
      rule,
      fields[0].0,
      format!("minimum {minimum} exceeds maximum {maximum}"),
    ));
  }
}

fn constant_number<T>(
  expression: Option<&Expression>,
  type_error: &str,
  convert: fn(f64) -> anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
  let Some(expression) = expression else {
    return Ok(None);
  };
  if expression_type(expression).is_some_and(|kind| kind != "number") {
    bail!("{type_error}");
  }
  folded_number(expression).map(convert).transpose()
}

fn constant_frequency(
  expression: Option<&Expression>,
) -> anyhow::Result<Option<Frequency>> {
  constant_number(
    expression,
    "frequency must be a number in MHz",
    Frequency::from_mhz,
  )
}

fn constant_percentage(
  expression: Option<&Expression>,
) -> anyhow::Result<Option<u8>> {
  constant_number(expression, "percentage must be a number", super::percentage)
}

fn constant_unsigned_integer(
  expression: Option<&Expression>,
) -> anyhow::Result<Option<u64>> {
  constant_number(
    expression,
    "value must be an unsigned integer",
    super::unsigned_integer,
  )
}

// Operator errors are reported by expression verification, so they are
// discarded here.
fn folded_number(expression: &Expression) -> Option<f64> {
  constants::number(expression).ok().flatten()
}

fn verify_shadowed_conditions(
  rules: &[Rule],
  diagnostics: &mut Vec<Diagnostic>,
) {
  let mut sorted: Vec<_> = rules.iter().collect();
  sorted.sort_by_key(|rule| Reverse(rule.priority));
  let mut seen: Vec<&Rule> = Vec::new();
  for rule in sorted {
    if constants::boolean(&rule.condition) == Some(false) {
      continue;
    }
    if let Some(previous) = seen.iter().find(|previous| shadows(previous, rule))
    {
      diagnostics.push(Diagnostic::warning(
        rule,
        "if",
        format!(
          "configured actions are shadowed by higher-priority rule {:?} \
           whenever this condition matches",
          previous.name
        ),
      ));
    }
    if shadow_candidate(rule, diagnostics) {
      seen.push(rule);
    }
  }
}

fn shadows(higher: &Rule, lower: &Rule) -> bool {
  higher.priority > lower.priority
    && (higher.condition == lower.condition
      || constants::boolean(&higher.condition) == Some(true))
    && actions_cover(higher, lower)
}

fn shadow_candidate(rule: &Rule, diagnostics: &[Diagnostic]) -> bool {
  !diagnostics.iter().any(|diagnostic| {
    diagnostic.rule == rule.name && diagnostic.severity == Severity::Error
  }) && expressions::rule_expressions(rule)
    .filter(|(field, _)| *field != "if")
    .all(|(_, expression)| {
      constants::analyze(expression).is_ok_and(|value| value.kind().is_some())
    })
}

// Dynamic selectors can select different devices, so selectors must be
// identical.
fn actions_cover(higher: &Rule, lower: &Rule) -> bool {
  let higher_fields: HashMap<_, _> =
    expressions::rule_expressions(higher).collect();
  let mut has_action = false;
  for (field, expression) in expressions::rule_expressions(lower) {
    if field == "if" {
      continue;
    }
    if field.ends_with(".for") {
      if higher_fields.get(field).copied() != Some(expression) {
        return false;
      }
    } else {
      has_action = true;
      if !higher_fields.contains_key(field) {
        return false;
      }
    }
  }
  has_action && same_selectors(higher, lower)
}

fn same_selectors(a: &Rule, b: &Rule) -> bool {
  selectors(a) == selectors(b)
}

fn selectors(rule: &Rule) -> [&Option<Expression>; 6] {
  [
    &rule.cpu.for_,
    &rule.disk.for_,
    &rule.usb.for_,
    &rule.gpu.for_,
    &rule.uncore.for_,
    &rule.power.for_,
  ]
}

#[cfg(test)]
mod tests {
  use std::{
    fs,
    time::Duration,
  };

  use super::*;

  pub(super) fn rule_config(fields: &str) -> DaemonConfig {
    toml::from_str(&format!("[[rule]]\nname='test'\npriority=1\n{fields}\n"))
      .unwrap()
  }

  pub(super) fn load_rule(fields: &str) -> anyhow::Result<DaemonConfig> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(
      &path,
      format!("[[rule]]\nname='test'\npriority=1\n{fields}\n"),
    )
    .unwrap();
    DaemonConfig::load_from(Some(&path))
  }

  pub(super) fn load_error(fields: &str) -> String {
    load_rule(fields).unwrap_err().to_string()
  }

  fn fields(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics
      .iter()
      .map(|diagnostic| diagnostic.field.as_str())
      .collect()
  }

  fn has_error(diagnostics: &[Diagnostic], field: &str) -> bool {
    diagnostics.iter().any(|diagnostic| {
      diagnostic.field == field && diagnostic.severity == Severity::Error
    })
  }

  #[test]
  fn cpu_selector_diagnostics_identify_invalid_items() {
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "invalid"
priority = 1
cpu.for = [0, -1, 0.5, 4294967296, "zero"]
[[rule]]
name = "scalar"
priority = 2
cpu.for = 0
[[rule]]
name = "dynamic"
priority = 3
cpu.for = [0, 0, "%cpu-core-count"]
"#,
    )
    .unwrap();
    let diagnostics = config.verify_static();
    assert_eq!(fields(&diagnostics), [
      "cpu.for[1]",
      "cpu.for[2]",
      "cpu.for[3]",
      "cpu.for[4]",
      "cpu.for"
    ]);
    assert_eq!(diagnostics[4].rule, "scalar");
  }

  #[test]
  fn device_selector_diagnostics_identify_invalid_literal_items() {
    for device in ["uncore", "disk", "usb", "gpu", "power"] {
      let diagnostics = rule_config(&format!(
        "{device}.for=['device0', 0, true, [], '$power-profile-preference']"
      ))
      .verify_static();
      assert_eq!(fields(&diagnostics), [
        format!("{device}.for[1]"),
        format!("{device}.for[2]"),
        format!("{device}.for[3]")
      ]);
      for selection in ["'device0'", "0", "true"] {
        let diagnostics =
          rule_config(&format!("{device}.for={selection}")).verify_static();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].field, format!("{device}.for"));
        assert!(diagnostics[0].message.contains("must be a list"));
      }
      assert!(
        rule_config(&format!(
          "{device}.for=['device0', '$power-profile-preference']"
        ))
        .verify_static()
        .is_empty()
      );
    }
  }

  #[test]
  fn bounds_reject_invalid_values_and_inverted_pairs() {
    for ([minimum_field, maximum_field], cases) in [
      (
        [
          "uncore.frequency-khz-minimum",
          "uncore.frequency-khz-maximum",
        ],
        &[
          ("800000", "3000000", 0),
          ("3000000", "800000", 1),
          ("-1", "18446744073709551616.0", 2),
          ("0.5", "inf", 2),
        ][..],
      ),
      (
        [
          "cpu.pstate-min-performance-percent",
          "cpu.pstate-max-performance-percent",
        ],
        &[
          ("0", "100", 0),
          ("100", "100", 0),
          ("-1", "101", 2),
          ("0.5", "nan", 2),
          ("'low'", "true", 2),
          ("90", "10", 1),
        ],
      ),
    ] {
      for (minimum, maximum, errors) in cases {
        let input =
          format!("{minimum_field}={minimum}\n{maximum_field}={maximum}");
        let diagnostics = rule_config(&input).verify_static();
        assert_eq!(diagnostics.len(), *errors, "{input}");
        assert!(diagnostics.iter().all(|diagnostic| {
          [minimum_field, maximum_field].contains(&diagnostic.field.as_str())
        }));
      }
    }
  }

  #[test]
  fn loading_rejects_invalid_charge_threshold_literals() {
    for field in ["power.charge-threshold-start", "power.charge-threshold-end"]
    {
      for value in ["-1", "101", "0.5", "nan", "inf", "'80'", "true", "[]"] {
        let error = load_error(&format!("{field}={value}"));
        assert!(error.contains(field), "{error}");
      }
    }
    for (start, end) in
      [("0", "100"), ("100", "0"), ("'$cpu-temperature'", "80")]
    {
      load_rule(&format!(
        "power.charge-threshold-start={start}\npower.\
         charge-threshold-end={end}"
      ))
      .unwrap();
    }
  }

  #[test]
  fn loading_rejects_literal_non_boolean_conditions() {
    for condition in ["0", "1", "'true'", "[]", "[true]"] {
      let error = load_error(&format!("if={condition}"));
      assert!(error.contains("priority 1"), "{error}");
      assert!(error.contains("if: condition must be a boolean"), "{error}");
    }
    for condition in ["true", "false", "'?discharging'"] {
      load_rule(&format!("if={condition}")).unwrap();
    }
  }

  #[test]
  fn loading_checks_types_and_selectors_of_constant_arithmetic() {
    for (field, expression) in [
      ("if", "{value=1,plus=1}"),
      ("cpu.turbo", "{value=1,minus=1}"),
      ("cpu.governor", "{value=1,plus=1}"),
      ("cpu.energy-performance-preference", "{value=255,plus=1}"),
      ("cpu.energy-performance-preference", "{value=1,divide=2}"),
      ("cpu.for", "[{value=0,minus=1}]"),
      ("cpu.for", "{value=1,plus=1}"),
      ("usb.for", "[{value=1,plus=1}]"),
      ("usb.for", "{value=1,plus=1}"),
      ("if", "'$cpu-temperature'"),
      ("cpu.turbo", "'$cpu-temperature'"),
      ("cpu.governor", "'?frequency-available'"),
      ("cpu.frequency-mhz-minimum", "'?discharging'"),
      ("power.charge-threshold-end", "'$power-profile-preference'"),
      ("cpu.for", "['$power-profile-preference']"),
      ("usb.for", "['%cpu-core-count']"),
    ] {
      let error = load_error(&format!("{field}={expression}"));
      assert!(error.contains(field), "{error}");
    }
    load_rule(
      r#"cpu.for=[{value=1,minus=1}]
cpu.energy-performance-preference={value=250,plus=5}
usb.for=[{if='?discharging',then='usb1',else='usb2'}]"#,
    )
    .unwrap();
    load_rule(
      r#"cpu.for={if='?discharging',then=[0],else=[1]}
cpu.energy-performance-preference={first-available-energy-performance-preference=['balance_power']}
usb.for={if='?discharging',then=['usb1'],else=['usb2']}"#,
    )
    .unwrap();
  }

  #[test]
  fn loading_checks_bounds_of_constant_arithmetic() {
    for (field, expression) in [
      ("power.charge-threshold-end", "{value=50,plus=60}"),
      ("vm.dirty-ratio", "{value=1,divide=2}"),
      ("audio.timeout-seconds", "{value=0,minus=1}"),
      ("cpu.frequency-mhz-minimum", "{value=0,minus=1}"),
      ("usb.autosuspend-delay-seconds", "{value=1e16,multiply=2}"),
      ("gpu.panel-power-savings", "{value=3,plus=2}"),
    ] {
      let error = load_error(&format!("{field}={expression}"));
      assert!(error.contains(field), "{error}");
    }
    let error = load_error(
      "cpu.frequency-mhz-minimum={value=1000,plus=1}\ncpu.\
       frequency-mhz-maximum=1000",
    );
    assert!(error.contains("exceeds maximum"), "{error}");
    load_rule(
      r#"power.charge-threshold-end={value=50,plus=50}
cpu.frequency-mhz-minimum={value=999,plus=1}
cpu.frequency-mhz-maximum=1000
audio.timeout-seconds={value='$cpu-temperature',minus=1}"#,
    )
    .unwrap();
  }

  #[test]
  fn loading_checks_hugepage_modes() {
    for (field, invalid) in [
      ("vm.transparent-hugepages", "defer"),
      ("vm.transparent-hugepage-defrag", "enabled"),
    ] {
      let error = load_error(&format!("{field}='{invalid}'"));
      assert!(error.contains(field), "{error}");
    }
    for (mode, defrag) in [
      ("always", "always"),
      ("madvise", "defer"),
      ("never", "defer+madvise"),
      ("always", "madvise"),
      ("always", "never"),
    ] {
      load_rule(&format!(
        "vm.transparent-hugepages='{mode}'\nvm.transparent-hugepage-defrag='\
         {defrag}'"
      ))
      .unwrap();
    }
  }

  #[test]
  fn loading_checks_numeric_limits() {
    let u64_maximum = "18446744073709549568.0";
    let u64_overflow = "18446744073709551616.0";
    for (field, maximum, overflow) in [
      ("cpu.dma-latency-us", "2147483647", "2147483648"),
      ("cpu.pm-qos-resume-latency-us", u64_maximum, u64_overflow),
      ("disk.readahead-kib", u64_maximum, u64_overflow),
      ("disk.apm", "255", "256"),
      ("disk.spindown", "255", "256"),
      ("audio.timeout-seconds", u64_maximum, u64_overflow),
      (
        "usb.autosuspend-delay-seconds",
        "18446744073709548.0",
        "18446744073709552.0",
      ),
      ("gpu.panel-power-savings", "4", "5"),
      ("vm.dirty-bytes", u64_maximum, u64_overflow),
      ("vm.dirty-background-bytes", u64_maximum, u64_overflow),
      ("vm.dirty-ratio", "100", "101"),
      ("vm.dirty-background-ratio", "100", "101"),
    ] {
      for invalid in ["-1", "0.5", "nan", "inf", "true", "'10'", "[]", overflow]
      {
        let error = load_error(&format!("{field}={invalid}"));
        assert!(error.contains(field), "{error}");
      }
      for valid in ["0", maximum, "'$cpu-temperature'"] {
        load_rule(&format!("{field}={valid}")).unwrap();
      }
    }
    load_rule("cpu.pm-qos-resume-latency-us='n/a'").unwrap();
  }

  #[test]
  fn loading_rejects_conflicting_literal_vm_limits() {
    for (bytes, ratio) in [
      ("vm.dirty-bytes", "vm.dirty-ratio"),
      ("vm.dirty-background-bytes", "vm.dirty-background-ratio"),
    ] {
      let error = load_error(&format!("{bytes}=0\n{ratio}=0"));
      assert!(error.contains(bytes) && error.contains(ratio), "{error}");
      for (byte_value, ratio_value) in
        [("0", "'$cpu-temperature'"), ("'$cpu-temperature'", "0")]
      {
        load_rule(&format!("{bytes}={byte_value}\n{ratio}={ratio_value}"))
          .unwrap();
      }
    }
  }

  #[test]
  fn loading_rejects_wrong_action_types_and_preserves_dynamic_values() {
    for (field, invalid, valid) in [
      ("cpu.governor", "true", "'powersave'"),
      ("cpu.energy-perf-bias", "6", "'6'"),
      ("cpu.turbo", "'true'", "true"),
      ("vm.transparent-hugepages", "[]", "'madvise'"),
      ("vm.transparent-hugepage-defrag", "0", "'defer'"),
      ("disk.scheduler", "false", "'none'"),
      ("disk.alpm", "1", "'min_power'"),
      ("usb.autosuspend", "1", "false"),
      ("audio.reset-controller", "[]", "true"),
      ("gpu.radeon-powersave", "true", "'dynpm'"),
      ("power.platform-profile", "0", "'balanced'"),
    ] {
      let error = load_error(&format!("{field}={invalid}"));
      assert!(error.contains(field), "{error}");
      assert!(error.contains("expected"), "{error}");
      load_rule(&format!("{field}={valid}")).unwrap();
    }
    load_rule(
      "cpu.turbo='?discharging'\ncpu.governor={first-available-governor=['\
       powersave','schedutil']}",
    )
    .unwrap();
  }

  #[test]
  fn loading_rejects_invalid_literal_frequencies() {
    for value in ["-1", "0", "nan", "inf", "'fast'", "true", "[]"] {
      let error = load_error(&format!("cpu.frequency-mhz-minimum={value}"));
      assert!(error.contains("cpu.frequency-mhz-minimum"), "{error}");
    }
    let error = load_error(
      "cpu.frequency-mhz-minimum=2000\ncpu.frequency-mhz-maximum=1000",
    );
    assert!(error.contains("exceeds maximum"), "{error}");
  }

  #[test]
  fn frequency_verification_uses_khz_rounding_and_defers_dynamic_values() {
    for fields in [
      "cpu.frequency-mhz-minimum=1000.0004\ncpu.frequency-mhz-maximum=1000",
      "cpu.frequency-mhz-minimum='$cpu-frequency-minimum'",
    ] {
      assert!(rule_config(fields).verify_static().is_empty());
    }
  }

  #[test]
  fn loading_checks_epp_types_and_integer_syntax() {
    for value in [
      "'1.0'", "'1e2'", "'-1'", "'256'", "true", "false", "[]", "[0]", "0.5",
      "256", "nan", "inf",
    ] {
      let error =
        load_error(&format!("cpu.energy-performance-preference={value}"));
      assert!(
        error.contains("cpu.energy-performance-preference"),
        "{error}"
      );
    }
    for value in [
      "0",
      "255",
      "'0'",
      "'255'",
      "'010'",
      "'balance_power'",
      "'$cpu-temperature'",
    ] {
      load_rule(&format!("cpu.energy-performance-preference={value}")).unwrap();
    }
  }

  #[test]
  fn reports_each_ambiguous_rule_identity() {
    let config: DaemonConfig = toml::from_str(
      r#"
[[rule]]
name = "base"
priority = 1
[[rule]]
name = "base"
priority = 2
[[rule]]
name = "boost"
priority = 2
"#,
    )
    .unwrap();
    let diagnostics = config.verify_rule_identities();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].rule, "base");
    assert_eq!(diagnostics[0].field, "name");
    assert_eq!(diagnostics[1].rule, "boost");
    assert_eq!(diagnostics[1].field, "priority");
    assert!(diagnostics[1].message.contains("base"));
    let error = config
      .verify_static()
      .iter()
      .map(ToString::to_string)
      .collect::<String>();
    assert!(error.contains("name is also used"));
    assert!(error.contains("priority is also used"));
  }

  #[test]
  fn folded_conditionals_validate_types_bounds_and_selector_items() {
    let diagnostics = rule_config(
      "cpu.for={if=true,then=[{maximum=[0,4294967296]}],else=[]}\ncpu.\
       frequency-mhz-minimum={if={value=2,is-more-than=1},then={maximum=[3000,\
       4000]},else=0}\ncpu.frequency-mhz-maximum={minimum=[1000,2000]}\nusb.\
       for={if=true,then=[1],else=['valid']}\nusb.autosuspend={if=true,then='\
       wrong',else=false}",
    )
    .verify_static();
    for field in [
      "cpu.for[0]",
      "cpu.frequency-mhz-minimum",
      "usb.for[0]",
      "usb.autosuspend",
    ] {
      assert!(has_error(&diagnostics, field), "{field}: {diagnostics:?}");
    }
  }

  #[test]
  fn unreachable_pure_branches_do_not_create_action_errors() {
    for fields in [
      "cpu.frequency-mhz-minimum={if=false,then={value=1,divide=0},\
       else={minimum=[800,900]}}\ncpu.pm-qos-resume-latency-us={if=true,then='\
       n/a',else=-1}",
      "cpu.frequency-mhz-minimum={if=true,then='$cpu-temperature',\
       else={load-average-since=3}}",
    ] {
      let diagnostics = rule_config(fields).verify_static();
      assert!(
        diagnostics
          .iter()
          .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{diagnostics:?}"
      );
    }
  }

  #[test]
  fn constant_conditions_warn_and_shadowing_respects_priority_and_coverage() {
    let config: DaemonConfig = toml::from_str(
      r#"[[rule]]
name='low'
priority=1
cpu.turbo=false
[[rule]]
name='high'
priority=3
if={not=false}
cpu.turbo=true
[[rule]]
name='partial'
priority=2
cpu.turbo=false
audio.timeout-seconds=10
[[rule]]
name='never'
priority=4
if={any=[false,{value=2,is-less-than=1}]}
cpu.turbo=true"#,
    )
    .unwrap();
    let diagnostics = config.verify_static();
    let reported = |rule: &str, message: &str| {
      diagnostics.iter().any(|diagnostic| {
        diagnostic.rule == rule && diagnostic.message.contains(message)
      })
    };
    assert!(reported("high", "always evaluates to true"));
    assert!(reported("never", "never matches"));
    assert!(reported("low", "shadowed"));
    assert!(!reported("partial", "shadowed"));
  }

  #[test]
  fn duration_windows_warn_about_retention_without_rejecting_one_sample() {
    for window in ["0s", "49m31s", "1h"] {
      let diagnostics = rule_config(&format!(
        "cpu.frequency-mhz-minimum={{cpu-usage-since='{window}'}}"
      ))
      .verify_static();
      assert_eq!(diagnostics.len(), 1, "{window}: {diagnostics:?}");
      assert_eq!(diagnostics[0].severity, Severity::Warning);
    }
    for window in ["500ms", "1s", "30s", "60s", "100s", "5m", "49m30s"] {
      let diagnostics = rule_config(&format!(
        "cpu.frequency-mhz-minimum={{load-average-since='{window}'}}"
      ))
      .verify_static();
      assert!(diagnostics.is_empty(), "{window}: {diagnostics:?}");
    }
  }

  #[test]
  fn dynamic_duration_expressions_reject_known_non_string_results() {
    for duration in [
      "'%cpu-core-count'",
      "{value='%cpu-core-count',plus=1}",
      "{if='?discharging',then=1,else=2}",
      "'?discharging'",
    ] {
      for operator in ["cpu-usage-since", "load-average-since"] {
        let fields =
          format!("cpu.frequency-mhz-minimum={{{operator}={duration}}}");
        let diagnostics = rule_config(&fields).verify_static();
        assert!(
          has_error(&diagnostics, "cpu.frequency-mhz-minimum"),
          "{fields}: {diagnostics:?}"
        );
      }
    }
    let diagnostics = rule_config(
      "cpu.frequency-mhz-minimum={cpu-usage-since={if='?discharging',then='1s'\
       ,else='2s'}}",
    )
    .verify_static();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
  }

  #[test]
  fn pure_numeric_failures_are_reported_once() {
    for fields in [
      "cpu.pstate-max-performance-percent=nan",
      "uncore.frequency-khz-maximum=inf",
      "power.charge-threshold-start=nan",
      "cpu.frequency-mhz-minimum={value=1,divide=0}",
    ] {
      let diagnostics = rule_config(fields).verify_static();
      assert_eq!(diagnostics.len(), 1, "{fields}: {diagnostics:?}");
      assert_eq!(diagnostics[0].severity, Severity::Error);
      assert_eq!(diagnostics[0].field, fields.split('=').next().unwrap());
    }
  }

  #[test]
  fn constant_duration_folds_selected_strings_and_defers_others() {
    let expression = |text: &str| toml::from_str(text).unwrap();
    assert_eq!(
      constant_duration(&expression(
        "if={not=false}\nthen='500ms'\nelse='invalid'"
      )),
      Some(Duration::from_millis(500))
    );
    assert_eq!(
      constant_duration(&expression("if='?discharging'\nthen='1s'\nelse='2s'")),
      None
    );
    assert_eq!(constant_duration(&expression("value=1\nplus=2")), None);
    assert_eq!(
      constant_duration(&Expression::String("invalid".into())),
      None
    );
  }
}
