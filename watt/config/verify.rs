use std::{
  collections::HashMap,
  fmt,
};

use anyhow::bail;
use serde::Serialize;

use super::{
  CpusDelta,
  DaemonConfig,
  Expression,
  Rule,
};
use crate::cpu::frequency::Frequency;

mod constants;
mod hardware;

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
  /// Checks rule identities and literal bounds without probing hardware.
  pub fn verify_static(&self) -> Vec<Diagnostic> {
    let mut diagnostics = self.verify_rule_identities();
    for rule in &self.rules {
      verify_cpu_selection(rule, &mut diagnostics);
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

fn verify_cpu_selection(rule: &Rule, diagnostics: &mut Vec<Diagnostic>) {
  match &rule.cpu.for_ {
    Some(Expression::List(values)) => {
      for (index, expression) in values.iter().enumerate() {
        if let Err(error) = literal_cpu_number(expression) {
          diagnostics.push(Diagnostic::error(
            rule,
            format!("cpu.for[{index}]"),
            error.to_string(),
          ));
        }
      }
    },
    Some(
      Expression::Number(_) | Expression::String(_) | Expression::Boolean(_),
    ) => {
      diagnostics.push(Diagnostic::error(
        rule,
        "cpu.for",
        "CPU selection must be a list",
      ));
    },
    _ => {},
  }
}

fn literal_cpu_number(expression: &Expression) -> anyhow::Result<()> {
  match expression {
    Expression::Number(value) => {
      CpusDelta::cpu_number(Expression::Number(*value)).map(|_| ())
    },
    Expression::String(_) | Expression::Boolean(_) | Expression::List(_) => {
      bail!("CPU selection item must be a number")
    },
    _ => Ok(()),
  }
}

fn literal_epp_error(expression: Option<&Expression>) -> Option<String> {
  let invalid = match expression? {
    Expression::Number(value) => {
      !super::unsigned_integer(*value)
        .is_ok_and(|value| value <= u64::from(u8::MAX))
    },
    Expression::String(value) => {
      value.parse::<f64>().is_ok() && value.parse::<u8>().is_err()
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
  match expression {
    Some(Expression::Number(value)) => convert(*value).map(Some),
    Some(
      Expression::String(_) | Expression::Boolean(_) | Expression::List(_),
    ) => bail!("{type_error}"),
    _ => Ok(None),
  }
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

#[cfg(test)]
mod tests {
  use std::fs;

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
  fn loading_checks_epp_integer_syntax() {
    for value in [
      "'1.0'", "'1e2'", "'-1'", "'256'", "0.5", "256", "nan", "inf",
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
}
