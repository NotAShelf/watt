use std::cmp::Ordering;

use anyhow::{
  bail,
  ensure,
};

use super::Expression;

/// Constant expression results. Telemetry-dependent expressions are `Unknown`.
#[derive(Debug, PartialEq)]
pub(super) enum Constant<'a> {
  Unknown,
  Unset,
  Boolean(bool),
  Number(f64),
  String(&'a str),
  List(Vec<Constant<'a>>),
}

impl Constant<'_> {
  fn number(self) -> anyhow::Result<Option<f64>> {
    match self {
      Self::Number(value) => Ok(Some(value)),
      Self::Unknown | Self::Unset => Ok(None),
      _ => bail!("expected number operand"),
    }
  }

  fn boolean(self) -> anyhow::Result<Option<bool>> {
    match self {
      Self::Boolean(value) => Ok(Some(value)),
      Self::Unknown | Self::Unset => Ok(None),
      _ => bail!("expected boolean operand"),
    }
  }
}

pub(super) fn analyze(expression: &Expression) -> anyhow::Result<Constant<'_>> {
  analyze_bounded(expression, &mut 256, 0)
}

pub(super) fn boolean(expression: &Expression) -> Option<bool> {
  analyze(expression).ok()?.boolean().ok().flatten()
}

pub(super) fn active_expression(
  mut expression: &Expression,
) -> Option<&Expression> {
  for _ in 0..64 {
    let Expression::IfElse {
      condition,
      consequence,
      alternative,
    } = expression
    else {
      return Some(expression);
    };
    expression = match boolean(condition)? {
      true => consequence,
      false => alternative.as_deref()?,
    };
  }
  None
}

pub(super) fn strings(expression: &Expression) -> Option<Vec<&str>> {
  let Constant::List(values) = analyze(expression).ok()? else {
    return None;
  };
  values
    .into_iter()
    .map(|value| {
      match value {
        Constant::String(value) => Some(value),
        _ => None,
      }
    })
    .collect()
}

fn analyze_bounded<'a>(
  expression: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  use Expression::*;
  if *remaining == 0 || depth >= 64 {
    return Ok(Constant::Unknown);
  }
  *remaining -= 1;
  let depth = depth + 1;
  match expression {
    Boolean(_) | Number(_) | String(_) | List(_) => {
      literal(expression, remaining, depth)
    },
    Plus { a, b }
    | Minus { a, b }
    | Multiply { a, b }
    | Divide { a, b }
    | Power { a, b } => arithmetic(expression, a, b, remaining, depth),
    LessThan { a, b } | MoreThan { a, b } | Equal { a, b, .. } => {
      comparison(expression, a, b, remaining, depth)
    },
    And { .. } | Or { .. } | All { .. } | Any { .. } => {
      aggregate(expression, remaining, depth)
    },
    Minimum { numbers } | Maximum { numbers } => {
      extrema(expression, numbers, remaining, depth)
    },
    _ => special(expression, remaining, depth),
  }
}

fn literal<'a>(
  expression: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  match expression {
    Expression::Boolean(value) => Ok(Constant::Boolean(*value)),
    Expression::Number(value) => {
      ensure!(value.is_finite(), "constant number is non-finite");
      Ok(Constant::Number(*value))
    },
    Expression::String(value) => Ok(Constant::String(value)),
    Expression::List(values) => list(values, remaining, depth),
    _ => unreachable!(),
  }
}

fn list<'a>(
  values: &'a [Expression],
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  if values.len() > *remaining {
    return Ok(Constant::Unknown);
  }
  let mut result = Vec::with_capacity(values.len());
  for value in values {
    let value = analyze_bounded(value, remaining, depth)?;
    if matches!(value, Constant::Unknown | Constant::Unset) {
      return Ok(value);
    }
    result.push(value);
  }
  Ok(Constant::List(result))
}

fn arithmetic<'a>(
  expression: &Expression,
  a: &'a Expression,
  b: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  let mut numbers = [0.0; 2];
  for (index, operand) in [a, b].into_iter().enumerate() {
    let value = analyze_bounded(operand, remaining, depth)?;
    if matches!(value, Constant::Unknown | Constant::Unset) {
      return Ok(value);
    }
    numbers[index] = value.number()?.unwrap();
  }
  expression
    .calculate_arithmetic(numbers[0], numbers[1])
    .map(Constant::Number)
}

fn comparison<'a>(
  expression: &'a Expression,
  a: &'a Expression,
  b: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  let leeway = match expression {
    Expression::Equal { leeway, .. } => Some(leeway.as_ref()),
    _ => None,
  };
  let mut numbers = [0.0; 3];
  for (index, operand) in [Some(a), Some(b), leeway].into_iter().enumerate() {
    let Some(operand) = operand else {
      continue;
    };
    let value = analyze_bounded(operand, remaining, depth)?;
    if matches!(value, Constant::Unknown | Constant::Unset) {
      return Ok(value);
    }
    numbers[index] = value.number()?.unwrap();
  }
  Ok(Constant::Boolean(
    expression.calculate_comparison(numbers[0], numbers[1], numbers[2]),
  ))
}

fn aggregate<'a>(
  expression: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  match expression {
    Expression::And { a, b } => {
      logic([a.as_ref(), b.as_ref()], false, remaining, depth)
    },
    Expression::Or { a, b } => {
      logic([a.as_ref(), b.as_ref()], true, remaining, depth)
    },
    Expression::All { all } => logic(all, false, remaining, depth),
    Expression::Any { any } => logic(any, true, remaining, depth),
    _ => unreachable!(),
  }
}

fn logic<'a>(
  values: impl IntoIterator<Item = &'a Expression>,
  decisive: bool,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  for value in values {
    let value = analyze_bounded(value, remaining, depth)?;
    if matches!(value, Constant::Unknown | Constant::Unset) {
      return Ok(value);
    }
    if value.boolean()?.unwrap() == decisive {
      return Ok(Constant::Boolean(decisive));
    }
  }
  Ok(Constant::Boolean(!decisive))
}

fn special<'a>(
  expression: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  match expression {
    Expression::Not { .. } | Expression::IsUnset { .. } => {
      unary(expression, remaining, depth)
    },
    Expression::IfElse {
      condition,
      consequence,
      alternative,
    } => {
      conditional(
        condition,
        consequence,
        alternative.as_deref(),
        remaining,
        depth,
      )
    },
    _ => Ok(Constant::Unknown),
  }
}

fn unary<'a>(
  expression: &'a Expression,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  match expression {
    Expression::Not { not } => {
      let value = analyze_bounded(not, remaining, depth)?;
      if matches!(value, Constant::Unknown | Constant::Unset) {
        return Ok(value);
      }
      Ok(Constant::Boolean(!value.boolean()?.unwrap()))
    },
    Expression::IsUnset { a } => {
      Ok(match analyze_bounded(a, remaining, depth)? {
        Constant::Unknown => Constant::Unknown,
        Constant::Unset => Constant::Boolean(true),
        _ => Constant::Boolean(false),
      })
    },
    _ => unreachable!(),
  }
}

fn conditional<'a>(
  condition: &'a Expression,
  consequence: &'a Expression,
  alternative: Option<&'a Expression>,
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  let value = analyze_bounded(condition, remaining, depth)?;
  if matches!(value, Constant::Unknown | Constant::Unset) {
    return Ok(value);
  }
  if value.boolean()?.unwrap() {
    return analyze_bounded(consequence, remaining, depth);
  }
  match alternative {
    Some(value) => analyze_bounded(value, remaining, depth),
    None => Ok(Constant::Unset),
  }
}

fn extrema<'a>(
  expression: &Expression,
  values: &'a [Expression],
  remaining: &mut usize,
  depth: usize,
) -> anyhow::Result<Constant<'a>> {
  ensure!(
    !values.is_empty(),
    "minimum/maximum requires at least one expression"
  );
  let compare: fn(&f64, &f64) -> Ordering =
    if matches!(expression, Expression::Minimum { .. }) {
      f64::total_cmp
    } else {
      |a, b| b.total_cmp(a)
    };
  let mut result: Option<f64> = None;
  for value in values {
    let value = analyze_bounded(value, remaining, depth)?;
    if matches!(value, Constant::Unknown | Constant::Unset) {
      return Ok(value);
    }
    let value = value.number()?.unwrap();
    result = Some(Expression::select_extreme(value, result, compare));
  }
  Ok(Constant::Number(result.unwrap()))
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::*;
  use crate::config::test_support::with_cpu_state;

  fn parse(text: &str) -> Expression {
    toml::from_str(text).unwrap()
  }

  #[test]
  fn constants_match_runtime_arithmetic_comparison_logic_and_extrema() {
    with_cpu_state(&HashSet::new(), |state| {
      for text in [
        "value=3\nplus=2",
        "value=3\nminus=2",
        "value=3\nmultiply=2",
        "value=3\ndivide=2",
        "value=3\npower=2",
        "value=3\nis-less-than=2",
        "value=3\nis-more-than=2",
        "value=3\nis-equal=3\nleeway=0.1",
        "minimum=[-0.0,0.0]",
        "maximum=[-0.0,0.0]",
        "all=[true,{not=false}]",
        "any=[false,true]",
        "if=false\nthen=1",
        "is-unset={if=false,then=1}",
      ] {
        let expression = parse(text);
        let observed = expression.eval(state).unwrap();
        match analyze(&expression).unwrap() {
          Constant::Boolean(value) => {
            assert_eq!(observed, Some(Expression::Boolean(value)))
          },
          Constant::Number(value) => {
            assert_eq!(observed, Some(Expression::Number(value)))
          },
          Constant::Unset => assert_eq!(observed, None),
          _ => panic!("test expression must be scalar and constant"),
        }
      }
    });
  }

  #[test]
  fn equality_uses_runtime_strict_leeway() {
    assert_eq!(
      boolean(&parse("value = 1\nis-equal = 1\nleeway = 0")),
      Some(false)
    );
    assert_eq!(
      boolean(&parse("value = 1\nis-equal = 1\nleeway = 0.1")),
      Some(true)
    );
  }

  #[test]
  fn short_circuit_respects_unknown_before_decisive_value() {
    assert_eq!(
      boolean(&parse("value = false\nand = '%cpu-usage'")),
      Some(false)
    );
    assert_eq!(boolean(&parse("value = '%cpu-usage'\nand = false")), None);
  }

  #[test]
  fn unreachable_branch_does_not_poison_constant() {
    let value = parse(
      "if = false\nthen = { value = 1, divide = 0 }\nelse = { maximum = [2, \
       4] }",
    );
    assert_eq!(analyze(&value).unwrap(), Constant::Number(4.0));
  }

  #[test]
  fn analysis_is_bounded() {
    let mut value = Expression::Boolean(true);
    for _ in 0..128 {
      value = Expression::Not {
        not: Box::new(value),
      };
    }
    assert_eq!(boolean(&value), None);
  }
}
