use std::{
  fmt::Write as _,
  process,
};

fn main() {
  let Err(error) = watt::main() else {
    return;
  };
  let mut message = String::new();

  for (index, error) in error.chain().enumerate() {
    message.clear();

    if index > 0 {
      message.push_str("cause: ");
    }

    let error = error.to_string();
    let mut chars = error.char_indices();

    if let Some((_, first)) = chars.next()
      && let Some((second_start, second)) = chars.next()
      && second.is_lowercase()
    {
      let _ = write!(
        message,
        "{first_lowercase}{rest}",
        first_lowercase = first.to_lowercase(),
        rest = &error[second_start..],
      );
    } else {
      message.push_str(&error);
    };

    log::error!("{message}");
  }

  process::exit(1);
}
