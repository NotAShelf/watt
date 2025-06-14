use std::{
  fmt::Write as _,
  io,
  io::Write as _,
  process,
};

use yansi::Paint as _;

fn main() {
  let Err(error) = watt::main() else {
    return;
  };

  let mut err = io::stderr();

  let mut message = String::new();
  let mut chain = error.chain().rev().peekable();

  while let Some(error) = chain.next() {
    let _ = write!(
      err,
      "{header} ",
      header = if chain.peek().is_none() {
        "error:"
      } else {
        "cause:"
      }
      .red()
      .bold(),
    );

    String::clear(&mut message);
    let _ = write!(message, "{error}");

    let mut chars = message.char_indices();

    let _ = match (chars.next(), chars.next()) {
      (Some((_, first)), Some((second_start, second)))
        if second.is_lowercase() =>
      {
        writeln!(
          err,
          "{first_lowercase}{rest}",
          first_lowercase = first.to_lowercase(),
          rest = &message[second_start..],
        )
      },

      _ => {
        writeln!(err, "{message}")
      },
    };
  }

  process::exit(1);
}
