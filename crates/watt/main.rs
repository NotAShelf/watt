use std::{
  error::Error,
  fmt::Write as _,
  process,
};

fn main() {
  let Err(error) = watt::main() else {
    return;
  };
  let mut message = String::new();

  for (index, error) in error
    .contexts()
    .iter()
    .rev()
    .map(ToString::to_string)
    .chain(error.chain().map(format_error))
    .enumerate()
  {
    message.clear();

    if index > 0 {
      message.push_str("cause: ");
    }

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

fn format_error(error: &(dyn Error + 'static)) -> String {
  if let Some(error) = error.downcast_ref::<dbus::Error>()
    && let Some(name) = error.name()
  {
    format!("{name}: {error}")
  } else {
    error.to_string()
  }
}
