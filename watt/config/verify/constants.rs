use anyhow::{
  bail,
  ensure,
};

use super::Expression;
use crate::config::eval::{
  Evaluated,
  Evaluation,
  Evaluator,
  evaluate,
};

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
  pub(super) fn kind(&self) -> Option<&'static str> {
    match self {
      Self::Boolean(_) => Some("boolean"),
      Self::Number(_) => Some("number"),
      Self::String(_) => Some("string"),
      Self::List(_) => Some("list"),
      Self::Unknown | Self::Unset => None,
    }
  }
}

pub(super) fn analyze(
  expression: &Expression,
) -> anyhow::Result<Constant<'_>> {
  let mut analyzer = Analyzer {
    remaining: 256,
    depth:     0,
  };
  Ok(match analyzer.eval(expression)? {
    Evaluated::Value(value) => value,
    Evaluated::Unset => Constant::Unset,
    Evaluated::Unknown => Constant::Unknown,
  })
}

pub(super) fn number(expression: &Expression) -> anyhow::Result<Option<f64>> {
  match analyze(expression)? {
    Constant::Unknown | Constant::Unset => Ok(None),
    value => Analyzer::into_number(value).map(Some),
  }
}

pub(super) fn boolean(expression: &Expression) -> Option<bool> {
  match analyze(expression).ok()? {
    Constant::Boolean(value) => Some(value),
    _ => None,
  }
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

struct Analyzer {
  remaining: usize,
  depth:     usize,
}

impl<'a> Evaluator<'a> for Analyzer {
  type Value = Constant<'a>;

  fn eval(&mut self, expression: &'a Expression) -> Evaluation<Constant<'a>> {
    if self.remaining == 0 || self.depth >= 64 {
      return Ok(Evaluated::Unknown);
    }
    self.remaining -= 1;
    self.depth += 1;
    let result = evaluate(expression, self);
    self.depth -= 1;
    result
  }

  fn leaf(&mut self, expression: &'a Expression) -> Evaluation<Constant<'a>> {
    let value = match expression {
      Expression::Boolean(value) => Constant::Boolean(*value),
      Expression::Number(value) => {
        ensure!(value.is_finite(), "constant number is non-finite");
        Constant::Number(*value)
      },
      Expression::String(value) => Constant::String(value),
      _ => return Ok(Evaluated::Unknown),
    };
    Ok(Evaluated::Value(value))
  }

  fn boolean(value: bool) -> Constant<'a> {
    Constant::Boolean(value)
  }

  fn number(value: f64) -> Constant<'a> {
    Constant::Number(value)
  }

  fn list(values: Vec<Constant<'a>>) -> Constant<'a> {
    Constant::List(values)
  }

  fn into_boolean(value: Constant<'a>) -> anyhow::Result<bool> {
    match value {
      Constant::Boolean(value) => Ok(value),
      _ => bail!("expected boolean operand"),
    }
  }

  fn into_number(value: Constant<'a>) -> anyhow::Result<f64> {
    match value {
      Constant::Number(value) => Ok(value),
      _ => bail!("expected number operand"),
    }
  }

  fn empty_extrema(_name: &str) -> Evaluation<Constant<'a>> {
    bail!("minimum/maximum requires at least one expression");
  }

  fn list_fits(&self, len: usize) -> bool {
    len <= self.remaining
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse(text: &str) -> Expression {
    toml::from_str(text).unwrap()
  }

  #[test]
  fn boolean_folding_preserves_short_circuit_order() {
    assert_eq!(
      boolean(&parse("value = false\nand = '%cpu-usage'")),
      Some(false)
    );
    assert_eq!(boolean(&parse("value = '%cpu-usage'\nand = false")), None);
  }

  #[test]
  fn constant_folding_skips_unreachable_branches() {
    let value = parse(
      "if = false\nthen = { value = 1, divide = 0 }\nelse = { maximum = [2, \
       4] }",
    );
    assert_eq!(number(&value).unwrap(), Some(4.0));
  }

  #[test]
  fn unknown_values_are_not_known_to_be_unset() {
    let expression = parse("is-unset = ['$cpu-temperature']");
    assert_eq!(analyze(&expression).unwrap(), Constant::Unknown);
    let expression = parse("is-unset = [{ if = false, then = 1 }]");
    assert_eq!(analyze(&expression).unwrap(), Constant::Boolean(true));
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
