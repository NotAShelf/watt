#[cfg(test)] use std::cell::RefCell;
use std::{
  env,
  error,
  fs,
  io,
  path::{
    Path,
    PathBuf,
  },
  str,
  sync::OnceLock,
};

use anyhow::Context;

static SYSTEM_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
#[cfg(test)]
thread_local! {
  static TEST_SYSTEM_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn system_root() -> Option<PathBuf> {
  #[cfg(test)]
  if let Some(root) = TEST_SYSTEM_ROOT.with(|root| root.borrow().clone()) {
    return Some(root);
  }

  SYSTEM_ROOT
    .get_or_init(|| env::var_os("WATT_SYSTEM_ROOT").map(PathBuf::from))
    .clone()
}

/// Resolves system paths under `WATT_SYSTEM_ROOT` when configured.
///
/// The default remains the real host filesystem. The alternate root is for
/// unprivileged tests and must contain the same absolute-path layout.
pub fn path(path: impl AsRef<Path>) -> PathBuf {
  let path = path.as_ref();
  let Some(root) = system_root() else {
    return path.to_owned();
  };

  if path.starts_with(&root)
    || !matches!(
      path.components().next(),
      Some(std::path::Component::RootDir)
    )
  {
    return path.to_owned();
  }

  root.join(path.strip_prefix("/").expect("absolute path has a root"))
}

pub fn exists(input: impl AsRef<Path>) -> bool {
  path(input).exists()
}

pub fn read_dir(
  input: impl AsRef<Path>,
) -> anyhow::Result<Option<fs::ReadDir>> {
  let path = path(input);

  match fs::read_dir(&path) {
    Ok(entries) => Ok(Some(entries)),

    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),

    Err(error) => {
      Err(error).context(format!(
        "failed to read directory '{path}'",
        path = path.display(),
      ))
    },
  }
}

pub fn read(input: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
  let path = path(input);

  match fs::read_to_string(&path) {
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

pub fn write(input: impl AsRef<Path>, value: &str) -> anyhow::Result<()> {
  let path = path(input);

  fs::write(&path, value).with_context(|| {
    format!(
      "failed to write '{value}' to '{path}'",
      path = path.display(),
    )
  })
}

#[cfg(test)]
pub struct SystemRootGuard {
  old: Option<PathBuf>,
}

#[cfg(test)]
pub fn set_system_root_for_tests(root: impl Into<PathBuf>) -> SystemRootGuard {
  SystemRootGuard {
    old: TEST_SYSTEM_ROOT
      .with(|configured| configured.replace(Some(root.into()))),
  }
}

#[cfg(test)]
impl Drop for SystemRootGuard {
  fn drop(&mut self) {
    TEST_SYSTEM_ROOT.with(|configured| {
      configured.replace(self.old.take());
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn resolves_system_paths_below_configured_root() {
    let root = PathBuf::from("/tmp/watt-test-root");
    let _root = set_system_root_for_tests(&root);

    assert_eq!(
      path("/sys/devices/system/cpu"),
      root.join("sys/devices/system/cpu")
    );
    assert_eq!(path("relative/path"), PathBuf::from("relative/path"));
    assert_eq!(
      path(root.join("sys/devices/system/cpu")),
      root.join("sys/devices/system/cpu")
    );
  }
}
