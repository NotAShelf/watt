use std::{
  error,
  fs,
  io,
  path::Path,
  str,
};

use anyhow::Context;

pub fn exists(path: impl AsRef<Path>) -> bool {
  let path = path.as_ref();

  path.exists()
}

pub fn read_dir(path: impl AsRef<Path>) -> anyhow::Result<Option<fs::ReadDir>> {
  let path = path.as_ref();

  match fs::read_dir(path) {
    Ok(entries) => Ok(Some(entries)),

    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),

    Err(error) => {
      Err(error).context(format!(
        "failed to read directory '{path}'",
        path = path.display()
      ))
    },
  }
}

pub fn read(path: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
  let path = path.as_ref();

  match fs::read_to_string(path) {
    Ok(string) => Ok(Some(string.trim().to_owned())),

    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),

    Err(error) => {
      Err(error)
        .context(format!("failed to read '{path}'", path = path.display()))
    },
  }
}

pub fn read_n<N: str::FromStr>(
  path: impl AsRef<Path>,
) -> anyhow::Result<Option<N>>
where
  N::Err: error::Error + Send + Sync + 'static,
{
  let path = path.as_ref();

  match read(path)? {
    Some(content) => {
      Ok(Some(content.trim().parse().with_context(|| {
        format!(
          "failed to parse contents of '{path}' as a unsigned number",
          path = path.display(),
        )
      })?))
    },

    None => Ok(None),
  }
}

pub fn write(path: impl AsRef<Path>, value: &str) -> anyhow::Result<()> {
  let path = path.as_ref();

  fs::write(path, value).with_context(|| {
    format!(
      "failed to write '{value}' to '{path}'",
      path = path.display(),
    )
  })
}
