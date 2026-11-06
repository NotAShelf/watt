use std::collections::HashSet;

use super::{
  Expression,
  test_support::with_cpu_state,
};

#[test]
fn evaluation_preserves_lazy_branches_and_undefined_values() {
  with_cpu_state(&HashSet::new(), |state| {
    for (source, expected) in [
      (
        "value = false\nand = '%cpu-usage'",
        Some(Expression::Boolean(false)),
      ),
      (
        "value = true\nor = '%cpu-usage'",
        Some(Expression::Boolean(true)),
      ),
      (
        "if = false\nthen = '%cpu-usage'\nelse = 7",
        Some(Expression::Number(7.0)),
      ),
      (
        "all = [false, '%cpu-usage']",
        Some(Expression::Boolean(false)),
      ),
      (
        "any = [true, '%cpu-usage']",
        Some(Expression::Boolean(true)),
      ),
      ("all = []", Some(Expression::Boolean(true))),
      ("any = []", Some(Expression::Boolean(false))),
      (
        "if = true\nthen = 7\nelse = '%cpu-usage'",
        Some(Expression::Number(7.0)),
      ),
      ("if = false\nthen = '%cpu-usage'", None),
      ("if = '$cpu-temperature'\nthen = '%cpu-usage'", None),
      ("value = '$cpu-temperature'\nand = '%cpu-usage'", None),
      ("value = '$cpu-temperature'\nor = '%cpu-usage'", None),
      ("all = ['$cpu-temperature', '%cpu-usage']", None),
      ("any = ['$cpu-temperature', '%cpu-usage']", None),
      ("maximum = [1, '$cpu-temperature', '%cpu-usage']", None),
      (
        "value = '$cpu-temperature'\nis-less-than = '%cpu-usage'",
        None,
      ),
      (
        "value = '$cpu-temperature'\nis-more-than = '%cpu-usage'",
        None,
      ),
      (
        "value = 1\nis-equal = '$cpu-temperature'\nleeway = '%cpu-usage'",
        None,
      ),
      ("is-governor-available = '$cpu-temperature'", None),
      (
        "first-available-governor = ['$cpu-temperature', '%cpu-usage']",
        None,
      ),
      ("is-platform-profile-available = '$cpu-temperature'", None),
      (
        "first-available-platform-profile = ['$cpu-temperature']",
        None,
      ),
      ("cpu-usage-since = '$cpu-temperature'", None),
      ("not = '$cpu-temperature'", None),
      ("value = '$cpu-temperature'\nplus = '%cpu-usage'", None),
      ("minimum = [1, '$cpu-temperature', '%cpu-usage']", None),
      (
        "is-unset = '$cpu-temperature'",
        Some(Expression::Boolean(true)),
      ),
      (
        "value = 1\nis-equal = 1\nleeway = 0",
        Some(Expression::Boolean(false)),
      ),
    ] {
      let expression: Expression = toml::from_str(source).unwrap();
      assert_eq!(expression.eval(state).unwrap(), expected, "{source}");
    }
    let list =
      Expression::List(vec![Expression::CpuTemperature, Expression::CpuUsage]);
    assert_eq!(list.eval(state).unwrap(), None);
    let wrong_type: Expression =
      toml::from_str("value = 'wrong'\nplus = '$cpu-temperature'").unwrap();
    assert!(
      wrong_type
        .eval(state)
        .unwrap_err()
        .to_string()
        .contains("cast")
    );
  });
}

#[test]
fn extrema_preserve_total_float_order_and_reject_empty_lists() {
  with_cpu_state(&HashSet::new(), |state| {
    let minimum: Expression = toml::from_str("minimum = [nan, 1]").unwrap();
    assert_eq!(minimum.eval(state).unwrap(), Some(Expression::Number(1.0)));
    let maximum: Expression = toml::from_str("maximum = [1, nan]").unwrap();
    assert!(
      maximum
        .eval(state)
        .unwrap()
        .unwrap()
        .try_into_number()
        .unwrap()
        .is_nan()
    );
    for source in ["minimum = []", "maximum = []"] {
      let expression: Expression = toml::from_str(source).unwrap();
      assert!(
        expression
          .eval(state)
          .unwrap_err()
          .to_string()
          .contains("at least 1 expression")
      );
    }
  });
}

#[test]
fn comparisons_preserve_strict_leeway_and_float_semantics() {
  with_cpu_state(&HashSet::new(), |state| {
    for (source, expected) in [
      ("value = 2\nis-less-than = 3", true),
      ("value = 2\nis-more-than = 3", false),
      ("value = nan\nis-less-than = 3", false),
      ("value = nan\nis-more-than = 3", false),
      ("value = 2\nis-equal = 1\nleeway = 1", false),
      ("value = 2\nis-equal = 3\nleeway = 1", false),
      ("value = 2\nis-equal = 2.5\nleeway = 1", true),
      ("value = 2\nis-equal = 2\nleeway = -1", false),
      ("value = nan\nis-equal = 2\nleeway = 1", false),
    ] {
      let expression: Expression = toml::from_str(source).unwrap();
      assert_eq!(
        expression.eval(state).unwrap(),
        Some(Expression::Boolean(expected)),
        "{source}",
      );
    }
  });
}

#[test]
fn extrema_distinguish_signed_zero_without_skipping_later_errors() {
  with_cpu_state(&HashSet::new(), |state| {
    let numbers = vec![Expression::Number(0.0), Expression::Number(-0.0)];
    let minimum = Expression::Minimum {
      numbers: numbers.clone(),
    };
    let maximum = Expression::Maximum { numbers };
    for (expression, expected) in [(minimum, -0.0_f64), (maximum, 0.0_f64)] {
      let result = expression
        .eval(state)
        .unwrap()
        .unwrap()
        .try_into_number()
        .unwrap();
      assert_eq!(result.to_bits(), expected.to_bits());
    }
    for source in [
      "minimum = [nan, '%cpu-usage']",
      "maximum = [nan, '%cpu-usage']",
      "first-available-governor = ['powersave', '%cpu-usage']",
    ] {
      let expression: Expression = toml::from_str(source).unwrap();
      assert!(expression.eval(state).is_err(), "{source}");
    }
  });
}

#[test]
fn arithmetic_rejects_non_finite_results() {
  with_cpu_state(&HashSet::new(), |state| {
    for source in ["value = 1\ndivide = 0", "value = 0\ndivide = 0"] {
      let expression: Expression = toml::from_str(source).unwrap();
      assert!(
        expression
          .eval(state)
          .unwrap_err()
          .to_string()
          .contains("non-finite"),
        "{source}",
      );
    }
  });
}
