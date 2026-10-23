#[cfg(test)] use std::cell::RefCell;
#[cfg(not(test))]
use std::fs::{
  read_to_string as read_raw,
  write as write_raw,
};
#[cfg(not(test))] use std::sync::Mutex;
use std::{
  collections::{
    BTreeMap,
    BTreeSet,
  },
  env,
  error,
  fs,
  io,
  path::{
    Component,
    Path,
    PathBuf,
  },
  str,
  sync::OnceLock,
};

use anyhow::{
  Context,
  bail,
};

#[cfg(test)]
use crate::test_support::{
  read_raw,
  write_raw,
};

static SYSTEM_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
#[cfg(not(test))]
static MANAGED_SETTINGS: OnceLock<Mutex<ManagedSettings>> = OnceLock::new();
#[cfg(test)]
thread_local! {
  static TEST_SYSTEM_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
  static TEST_MANAGED_SETTINGS: RefCell<ManagedSettings> = RefCell::new(ManagedSettings::default());
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ReadbackKind {
  #[default]
  Ordinary,
  DriverTranslated,
}

/// The rule whose action successfully requested a managed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOrigin {
  pub name:     String,
  pub priority: u16,
}

#[derive(Debug)]
struct ManagedSetting {
  original:      String,
  last_written:  String,
  observed:      Option<String>,
  readback_kind: ReadbackKind,
  generation:    u64,
  rule:          Option<RuleOrigin>,
}

impl ManagedSetting {
  fn record_write(&mut self, value: &str) {
    value.clone_into(&mut self.last_written);
    self.observed = None;
    self.readback_kind = ReadbackKind::Ordinary;
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSetting {
  pub path:      PathBuf,
  pub requested: String,
  pub observed:  Option<String>,
  pub rule:      Option<RuleOrigin>,
  readback_kind: ReadbackKind,
}

impl AppliedSetting {
  pub fn is_verified(&self) -> bool {
    self
      .observed
      .as_deref()
      .is_some_and(|observed| equivalent_value(observed, &self.requested))
  }

  pub fn verification(&self) -> &'static str {
    if self.is_verified() {
      return "verified";
    }

    if self.readback_kind == ReadbackKind::DriverTranslated {
      return "translated";
    }

    match self.observed {
      Some(_)
        if self.path.file_name().is_some_and(|name| {
          matches!(name.to_str(), Some("scaling_min_freq" | "scaling_max_freq"))
        }) =>
      {
        "clamped"
      },
      Some(_) => "mismatch",
      None => "unverified",
    }
  }
}

#[derive(Default, Debug)]
struct ManagedSettings {
  active:          bool,
  generation:      u64,
  settings:        BTreeMap<PathBuf, ManagedSetting>,
  rules:           BTreeMap<PathBuf, RuleOrigin>,
  scan_protection: BTreeMap<&'static str, BTreeSet<PathBuf>>,
}

/// Keep originals for previously discovered roots that still exist but could
/// not be scanned. Successful rediscovery or physical removal clears them.
pub(crate) fn protect_scan_failures(
  subsystem: &'static str,
  previous: &[PathBuf],
  current: impl IntoIterator<Item = PathBuf>,
) {
  let current: BTreeSet<_> = current.into_iter().map(path).collect();
  with_managed_settings(|settings| {
    let protected = settings.scan_protection.entry(subsystem).or_default();
    protected.extend(previous.iter().map(path));
    protected.retain(|root| {
      if current.contains(root) {
        return false;
      }
      if root.try_exists().unwrap_or(true) {
        return true;
      }
      settings.settings.retain(|path, _| !path.starts_with(root));
      false
    });
  });
}

fn retain_unavailable_settings(settings: &mut ManagedSettings) {
  for (path, setting) in &mut settings.settings {
    let protected = path.ancestors().any(|ancestor| {
      settings
        .scan_protection
        .values()
        .any(|roots| roots.contains(ancestor))
    });
    if protected {
      setting.generation = settings.generation;
    }
  }
}

pub(crate) fn set_setting_rules(rules: BTreeMap<PathBuf, RuleOrigin>) {
  with_managed_settings(|settings| {
    settings.rules = rules
      .into_iter()
      .map(|(input, rule)| (path(input), rule))
      .collect();
  });
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
    || !matches!(path.components().next(), Some(Component::RootDir))
  {
    return path.to_owned();
  }

  root.join(path.strip_prefix("/").expect("absolute path has a root"))
}

/// Resolves a system path and follows its symlinks, or returns `None` if
/// absent.
pub fn canonicalize(
  input: impl AsRef<Path>,
) -> anyhow::Result<Option<PathBuf>> {
  let path = path(input);
  match fs::canonicalize(&path) {
    Ok(path) => Ok(Some(path)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => {
      Err(error)
        .with_context(|| format!("failed to resolve '{}'", path.display()))
    },
  }
}

pub fn exists(input: impl AsRef<Path>) -> bool {
  path(input).exists()
}

pub(crate) fn known_missing(input: impl AsRef<Path>) -> bool {
  matches!(path(input).try_exists(), Ok(false))
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

/// Devices discovered in a sysfs directory and per-device scan failures.
#[derive(Debug)]
pub struct Inventory<T> {
  pub devices:  Vec<T>,
  pub failures: Vec<String>,
}

impl<T> Inventory<T> {
  /// Scans entries in name order. `scan` returns `None` for entries that are
  /// not devices of this kind.
  pub(crate) fn scan_entries(
    entries: fs::ReadDir,
    kind: &str,
    scan: impl Fn(fs::DirEntry) -> anyhow::Result<Option<T>>,
  ) -> Self {
    let mut entries: Vec<_> = entries.collect();
    entries
      .sort_by_key(|entry| entry.as_ref().ok().map(fs::DirEntry::file_name));

    let mut inventory = Self {
      devices:  Vec::new(),
      failures: Vec::new(),
    };
    for entry in entries {
      match entry
        .with_context(|| format!("failed to read {kind} entry"))
        .and_then(&scan)
      {
        Ok(Some(device)) => inventory.devices.push(device),
        Ok(None) => {},
        Err(error) => inventory.failures.push(format!("{error:#}")),
      }
    }
    inventory
  }

  /// Logs partial failures and fails only if every scan failed.
  pub(crate) fn into_devices(self, kind: &str) -> anyhow::Result<Vec<T>> {
    if self.devices.is_empty() && !self.failures.is_empty() {
      bail!("all {kind} scans failed: {}", self.failures.join("; "));
    }
    for failure in self.failures {
      log::warn!("{kind} discovery failed: {failure}");
    }
    Ok(self.devices)
  }
}

pub fn read(input: impl AsRef<Path>) -> anyhow::Result<Option<String>> {
  let path = path(input);

  match read_raw(&path) {
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
    .then(|| read_raw(&path).map(normalize_value))
    .transpose()
    .with_context(|| {
      format!("failed to read original value of '{}'", path.display())
    })?;

  write_raw(&path, value).with_context(|| {
    format!(
      "failed to write '{value}' to '{path}'",
      path = path.display(),
    )
  })?;

  if track {
    with_managed_settings(|settings| {
      let generation = settings.generation;
      let rule = settings.rules.get(&path).cloned();
      if let Some(setting) = settings.settings.get_mut(&path) {
        setting.record_write(value);
        setting.generation = generation;
        setting.rule = rule;
      } else if let Some(original) = original {
        settings.settings.insert(path, ManagedSetting {
          original,
          last_written: value.to_owned(),
          observed: None,
          readback_kind: ReadbackKind::Ordinary,
          generation,
          rule,
        });
      }
    });
  }

  Ok(())
}

pub fn observe(input: impl AsRef<Path>, observed: impl Into<String>) {
  record_observation(input, observed, ReadbackKind::Ordinary);
}

pub(crate) fn observe_translation(
  input: impl AsRef<Path>,
  observed: impl Into<String>,
) {
  record_observation(input, observed, ReadbackKind::DriverTranslated);
}

fn record_observation(
  input: impl AsRef<Path>,
  observed: impl Into<String>,
  readback_kind: ReadbackKind,
) {
  let path = path(input);
  let observed = observed.into();
  with_managed_settings(|settings| {
    if let Some(setting) = settings.settings.get_mut(&path) {
      setting.observed = Some(observed);
      setting.readback_kind = readback_kind;
    }
  });
}

pub fn keep_setting(input: impl AsRef<Path>, observed: impl Into<String>) {
  let path = path(input);
  let observed = observed.into();
  with_managed_settings(|settings| {
    let generation = settings.generation;
    let rule = settings.rules.get(&path).cloned();
    if let Some(setting) = settings.settings.get_mut(&path) {
      setting.generation = generation;
      setting.rule = rule;
      setting.observed = Some(observed);
      setting.readback_kind = ReadbackKind::Ordinary;
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
          path:          path.clone(),
          requested:     setting.last_written.clone(),
          observed:      setting.observed.clone(),
          rule:          setting.rule.clone(),
          readback_kind: setting.readback_kind,
        }
      })
      .collect()
  })
}

/// Whether Watt manages the path or a rule requested it this iteration.
pub(crate) fn is_managed(input: impl AsRef<Path>) -> bool {
  let path = path(input);
  with_managed_settings(|settings| {
    settings.settings.contains_key(&path) || settings.rules.contains_key(&path)
  })
}

pub fn manage_settings() -> SettingsGuard {
  SettingsGuard
}

pub fn begin_settings_iteration() {
  with_managed_settings(|settings| {
    settings.active = true;
    settings.generation = settings.generation.wrapping_add(1);
    settings.rules.clear();
    retain_unavailable_settings(settings);
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
    TEST_MANAGED_SETTINGS.with(|settings| operation(&mut settings.borrow_mut()))
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
  let mut settings = with_managed_settings(|managed| {
    let generation = managed.generation;
    managed
      .settings
      .iter()
      .filter(|(_, setting)| should_restore(setting, generation))
      .map(|(path, setting)| {
        (
          path.clone(),
          setting.original.clone(),
          setting
            .observed
            .as_ref()
            .unwrap_or(&setting.last_written)
            .clone(),
        )
      })
      .collect::<Vec<_>>()
  });
  settings.sort_by_key(|(path, original, owned)| {
    !restoration_expands_bounds(path, original, owned)
  });

  let mut restored = Vec::new();
  let failures = settings
    .into_iter()
    .filter_map(|(path, original, owned)| {
      match restore_setting(&path, &original, &owned) {
        Ok(()) => {
          restored.push(path);
          None
        },
        Err(error) => {
          Some(RestoreFailure {
            path,
            message: format!("{error:#}"),
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

fn restoration_expands_bounds(
  path: &Path,
  original: &str,
  owned: &str,
) -> bool {
  let (Ok(original), Ok(owned)) =
    (original.trim().parse::<u64>(), owned.trim().parse::<u64>())
  else {
    return false;
  };
  match path.file_name().and_then(|name| name.to_str()) {
    Some("scaling_min_freq" | "min_freq_khz" | "min_perf_pct") => {
      original < owned
    },
    Some("scaling_max_freq" | "max_freq_khz" | "max_perf_pct") => {
      original > owned
    },
    _ => false,
  }
}

fn restore_setting(
  path: &Path,
  original: &str,
  owned: &str,
) -> anyhow::Result<()> {
  let current =
    read_raw(path).context("failed to read setting before restoration")?;
  if !equivalent_value(&current, owned) {
    log::warn!(
      "not restoring '{}' because it changed outside Watt",
      path.display()
    );
    return Ok(());
  }

  write_raw(path, original).context("failed to restore setting")?;
  with_managed_settings(|managed| {
    if let Some(setting) = managed.settings.get_mut(path) {
      setting.record_write(original);
      setting.rule = None;
    }
  });
  let observed = read_raw(path).context("failed to verify restored setting")?;
  let restored = equivalent_value(&observed, original);
  observe(path, normalize_value(observed));
  if !restored {
    bail!("restored setting did not retain '{original}'");
  }
  Ok(())
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
    slice,
  };

  use super::*;
  use crate::test_support::SystemFixture;

  #[test]
  fn protected_device_disappearance_retires_originals_without_restore_failures()
  {
    let fixture = SystemFixture::new();
    let root = fixture.path("/sys/block/test");
    let control = root.join("queue/read_ahead_kb");
    fixture.write(&control, "128");
    let _managed = manage_settings();
    begin_settings_iteration();
    write(&control, "256").unwrap();
    protect_scan_failures("disk", slice::from_ref(&root), []);
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(read(&control).unwrap().as_deref(), Some("256"));

    stdfs::remove_dir_all(&root).unwrap();
    protect_scan_failures("disk", &[], []);
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert!(applied_settings().is_empty());
  }

  #[test]
  fn failed_restoration_readback_clears_the_former_rule_origin() {
    let fixture = SystemFixture::new();
    let control = fixture.path("/sys/test/control");
    fixture.write(&control, "original");
    let _managed = manage_settings();
    begin_settings_iteration();
    set_setting_rules(BTreeMap::from([(control.clone(), RuleOrigin {
      name:     "removed-rule".into(),
      priority: 10,
    })]));
    write(&control, "requested").unwrap();
    assert_eq!(
      applied_settings()[0].rule.as_ref().unwrap().name,
      "removed-rule"
    );
    fixture.clear_io(&control);
    fixture.fail_read_after_write(&control, io::ErrorKind::PermissionDenied);
    begin_settings_iteration();
    assert_eq!(restore_unmanaged_settings().len(), 1);
    let setting = applied_settings().remove(0);
    assert_eq!(setting.requested, "original");
    assert_eq!(setting.rule, None);
    fixture.clear_io(&control);
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(read(&control).unwrap().as_deref(), Some("original"));
  }

  #[test]
  fn unreadable_original_prevents_mutation_and_recovers_on_retry() {
    let fixture = SystemFixture::new();
    let control = fixture.path("/sys/test/control");
    fixture.write(&control, "original");
    let _managed = manage_settings();
    begin_settings_iteration();
    fixture.fail_read(&control, io::ErrorKind::PermissionDenied);
    assert!(write(&control, "requested").is_err());
    assert_eq!(fixture.write_count(&control), 0);
    assert!(applied_settings().is_empty());
    assert_eq!(stdfs::read_to_string(&control).unwrap(), "original");
    fixture.clear_io(&control);
    write(&control, "requested").unwrap();
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(control).unwrap(), "original");
  }

  #[test]
  fn restoration_orders_bounds_without_inverting_the_range() {
    for (low, high, next_low, next_high) in [(10, 20, 30, 40), (30, 40, 10, 20)]
    {
      let fixture = SystemFixture::new();
      let minimum = "/sys/test/scaling_min_freq";
      let maximum = "/sys/test/scaling_max_freq";
      fixture.write(minimum, low.to_string());
      fixture.write(maximum, high.to_string());
      fixture.enforce_bounds(minimum, maximum);
      let _settings = manage_settings();
      begin_settings_iteration();
      let writes = if next_low > high {
        [(maximum, next_high), (minimum, next_low)]
      } else {
        [(minimum, next_low), (maximum, next_high)]
      };
      for (path, value) in writes {
        write(path, &value.to_string()).unwrap();
      }
      begin_settings_iteration();
      assert!(restore_unmanaged_settings().is_empty());
      assert_eq!(read(minimum).unwrap(), Some(low.to_string()));
      assert_eq!(read(maximum).unwrap(), Some(high.to_string()));
      assert!(applied_settings().is_empty());
    }
  }

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
    keep_setting(&setting, "watt");
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(stdfs::read_to_string(&setting).unwrap(), "watt");

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

  #[test]
  fn distinguishes_frequency_clamping_from_a_mismatch() {
    let setting = AppliedSetting {
      path:          PathBuf::from("/sys/cpu/scaling_min_freq"),
      requested:     "1000".into(),
      observed:      Some("900".into()),
      readback_kind: ReadbackKind::Ordinary,
      rule:          None,
    };
    assert_eq!(setting.verification(), "clamped");

    let setting = AppliedSetting {
      path: PathBuf::from("/sys/cpu/scaling_governor"),
      ..setting
    };
    assert_eq!(setting.verification(), "mismatch");
  }

  #[test]
  fn translated_readback_is_not_exact_and_does_not_survive_new_observations() {
    let fixture = SystemFixture::new();
    let path = "/sys/energy_performance_preference";
    fixture.write(path, "balance_power");
    let _settings = manage_settings();
    begin_settings_iteration();
    write(path, "123").unwrap();
    observe_translation(path, "performance");
    let applied = applied_settings().remove(0);
    assert_eq!(applied.verification(), "translated");
    assert!(!applied.is_verified());
    observe(path, "unexpected");
    assert_eq!(applied_settings()[0].verification(), "mismatch");
    observe_translation(path, "performance");
    write(path, "124").unwrap();
    assert_eq!(applied_settings()[0].verification(), "unverified");
  }

  #[test]
  fn new_writes_discard_stale_readback_before_restoration() {
    let fixture = SystemFixture::new();
    fixture.write("/sys/setting", "original");
    let _settings = manage_settings();
    begin_settings_iteration();
    write("/sys/setting", "first").unwrap();
    observe("/sys/setting", "first");
    write("/sys/setting", "second").unwrap();
    assert!(applied_settings()[0].observed.is_none());

    fixture.write("/sys/setting", "first");
    begin_settings_iteration();
    assert!(restore_unmanaged_settings().is_empty());
    assert_eq!(read("/sys/setting").unwrap().as_deref(), Some("first"));
  }

  #[test]
  fn failed_restoration_is_verified_and_retried_independently() {
    for (result, remaining) in [
      (Err(io::ErrorKind::PermissionDenied), "watt"),
      (Ok("clamped"), "clamped"),
    ] {
      let fixture = SystemFixture::new();
      fixture.write("/sys/first", "original");
      fixture.write("/sys/second", "original");
      let _settings = manage_settings();
      begin_settings_iteration();
      write("/sys/first", "watt").unwrap();
      write("/sys/second", "watt").unwrap();
      fixture.write_result("/sys/first", result);

      begin_settings_iteration();
      let failures = restore_unmanaged_settings();
      assert_eq!(failures.len(), 1);
      assert_eq!(failures[0].path, fixture.path("/sys/first"));
      assert_eq!(read("/sys/first").unwrap().as_deref(), Some(remaining));
      assert_eq!(read("/sys/second").unwrap().as_deref(), Some("original"));

      fixture.clear_io("/sys/first");
      assert!(restore_unmanaged_settings().is_empty());
      assert_eq!(read("/sys/first").unwrap().as_deref(), Some("original"));
      assert!(applied_settings().is_empty());
    }
  }
}
