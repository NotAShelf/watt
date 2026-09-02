#[cfg(test)] use std::cell::RefCell;
#[cfg(not(test))] use std::sync::Mutex;
use std::{
  collections::BTreeMap,
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
#[cfg(not(test))]
static MANAGED_SETTINGS: OnceLock<Mutex<ManagedSettings>> = OnceLock::new();
#[cfg(test)]
thread_local! {
  static TEST_SYSTEM_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
  static TEST_MANAGED_SETTINGS: RefCell<ManagedSettings> = RefCell::new(ManagedSettings::default());
}

#[derive(Debug)]
struct ManagedSetting {
  original:     String,
  last_written: String,
  observed:     Option<String>,
  generation:   u64,
}

#[derive(Debug, Clone)]
pub struct AppliedSetting {
  pub path:      PathBuf,
  pub requested: String,
  pub observed:  Option<String>,
}

impl AppliedSetting {
  pub fn is_verified(&self) -> bool {
    self
      .observed
      .as_deref()
      .is_some_and(|observed| equivalent_value(observed, &self.requested))
  }
}

#[derive(Default, Debug)]
struct ManagedSettings {
  active:     bool,
  generation: u64,
  settings:   BTreeMap<PathBuf, ManagedSetting>,
}

#[derive(Debug, Clone)]
pub struct RestoreFailure {
  pub path:    PathBuf,
  pub message: String,
}

pub struct SettingsGuard;

impl Drop for SettingsGuard {
  fn drop(&mut self) {
    for failure in restore_all_settings() {
      log::error!("{}: {}", failure.path.display(), failure.message);
    }
  }
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
  let (track, capture_original) = with_managed_settings(|settings| {
    (settings.active, !settings.settings.contains_key(&path))
  });
  let original = (track && capture_original)
    .then(|| fs::read_to_string(&path).ok().map(normalize_value))
    .flatten();

  fs::write(&path, value).with_context(|| {
    format!(
      "failed to write '{value}' to '{path}'",
      path = path.display(),
    )
  })?;

  if track {
    with_managed_settings(|settings| {
      let generation = settings.generation;
      if let Some(setting) = settings.settings.get_mut(&path) {
        setting.last_written = value.to_owned();
        setting.generation = generation;
      } else if let Some(original) = original {
        settings.settings.insert(path, ManagedSetting {
          original,
          last_written: value.to_owned(),
          observed: None,
          generation,
        });
      }
    });
  }

  Ok(())
}

pub fn observe(input: impl AsRef<Path>, observed: impl Into<String>) {
  let path = path(input);
  let observed = observed.into();
  with_managed_settings(|settings| {
    if let Some(setting) = settings.settings.get_mut(&path) {
      setting.observed = Some(observed);
    }
  });
}

pub fn applied_settings() -> Vec<AppliedSetting> {
  with_managed_settings(|settings| {
    settings
      .settings
      .iter()
      .map(|(path, setting)| {
        AppliedSetting {
          path:      path.clone(),
          requested: setting.last_written.clone(),
          observed:  setting.observed.clone(),
        }
      })
      .collect()
  })
}

pub fn manage_settings() -> SettingsGuard {
  SettingsGuard
}

pub fn begin_settings_iteration() {
  with_managed_settings(|settings| {
    settings.active = true;
    settings.generation = settings.generation.wrapping_add(1);
  });
}

pub fn restore_unmanaged_settings() -> Vec<RestoreFailure> {
  restore_settings(|setting, generation| setting.generation < generation)
}

pub fn retire_settings_under(input: impl AsRef<Path>) {
  let path = path(input);
  with_managed_settings(|settings| {
    settings
      .settings
      .retain(|setting, _| !setting.starts_with(&path));
  });
}

fn restore_all_settings() -> Vec<RestoreFailure> {
  let failures = restore_settings(|_, _| true);
  with_managed_settings(|settings| settings.active = false);
  failures
}

fn with_managed_settings<T>(
  operation: impl FnOnce(&mut ManagedSettings) -> T,
) -> T {
  #[cfg(test)]
  {
    return TEST_MANAGED_SETTINGS
      .with(|settings| operation(&mut settings.borrow_mut()));
  }

  #[cfg(not(test))]
  {
    operation(
      &mut MANAGED_SETTINGS
        .get_or_init(|| Mutex::new(ManagedSettings::default()))
        .lock()
        .expect("managed settings lock poisoned"),
    )
  }
}

fn restore_settings(
  should_restore: impl Fn(&ManagedSetting, u64) -> bool,
) -> Vec<RestoreFailure> {
  let settings = with_managed_settings(|managed| {
    let generation = managed.generation;
    managed
      .settings
      .iter()
      .filter(|(_, setting)| should_restore(setting, generation))
      .map(|(path, setting)| {
        (
          path.clone(),
          setting.original.clone(),
          setting.last_written.clone(),
        )
      })
      .collect::<Vec<_>>()
  });

  let mut restored = Vec::new();
  let failures = settings
    .into_iter()
    .filter_map(|(path, original, last_written)| {
      match fs::read_to_string(&path) {
        Ok(current) if equivalent_value(&current, &last_written) => {
          match fs::write(&path, original) {
            Ok(()) => {
              restored.push(path);
              None
            },
            Err(error) => {
              Some(RestoreFailure {
                path,
                message: format!("failed to restore setting: {error}"),
              })
            },
          }
        },
        Ok(_) => {
          log::warn!(
            "not restoring '{}' because it changed outside Watt",
            path.display(),
          );
          restored.push(path);
          None
        },
        Err(error) => {
          Some(RestoreFailure {
            path,
            message: format!(
              "failed to read setting before restoration: {error}"
            ),
          })
        },
      }
    })
    .collect();

  with_managed_settings(|managed| {
    for path in restored {
      managed.settings.remove(&path);
    }
  });
  failures
}

fn normalize_value(value: String) -> String {
  value
    .split_whitespace()
    .find_map(|value| {
      value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    })
    .unwrap_or(value.trim())
    .to_owned()
}

fn equivalent_value(observed: &str, expected: &str) -> bool {
  normalize_value(observed.to_owned()) == expected.trim()
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
  use std::{
    env,
    fs as stdfs,
    process,
  };

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

  #[test]
  fn restores_only_settings_watt_still_owns() {
    let root =
      env::temp_dir().join(format!("watt-settings-fixture-{}", process::id(),));
    stdfs::create_dir_all(&root).unwrap();
    let setting = root.join("setting");
    stdfs::write(&setting, "original").unwrap();
    let _settings = manage_settings();

    begin_settings_iteration();
    write(&setting, "watt").unwrap();
    observe(&setting, "watt");
    let applied = applied_settings();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].requested, "watt");
    assert!(applied[0].is_verified());
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(&setting).unwrap(), "original");

    begin_settings_iteration();
    write(&setting, "watt").unwrap();
    stdfs::write(&setting, "external").unwrap();
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(&setting).unwrap(), "external");

    begin_settings_iteration();
    write(&setting, "watt").unwrap();
    retire_settings_under(&root);
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(&setting).unwrap(), "watt");

    drop(_settings);
    stdfs::remove_dir_all(root).unwrap();
  }
}
