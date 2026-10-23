use std::{
  cell::RefCell,
  cmp::Ordering,
  collections::BTreeMap,
  fmt::Write as _,
  fs,
  io,
  mem,
  os::unix::fs::symlink,
  path::{
    Path,
    PathBuf,
  },
  str,
};

use tempfile::TempDir;

use crate::fs::{
  SystemRootGuard,
  set_system_root_for_tests,
};

#[derive(Default)]
struct FileBehavior {
  writes:                 usize,
  bound:                  Option<(PathBuf, Ordering)>,
  read_error:             Option<io::ErrorKind>,
  read_error_after_write: Option<io::ErrorKind>,
  write_result:           Option<Result<String, io::ErrorKind>>,
}

thread_local! {
  static FILE_BEHAVIORS: RefCell<BTreeMap<PathBuf, FileBehavior>> = const { RefCell::new(BTreeMap::new()) };
}

/// An isolated system tree installed for the current test thread.
pub(crate) struct SystemFixture {
  // Restore the thread's root before deleting the directory.
  _guard:      SystemRootGuard,
  root:        TempDir,
  previous_io: BTreeMap<PathBuf, FileBehavior>,
}

impl SystemFixture {
  pub(crate) fn new() -> Self {
    let root = tempfile::tempdir().expect("create system fixture");
    for directory in ["sys/block", "sys/bus/usb/devices", "sys/class/drm"] {
      fs::create_dir_all(root.path().join(directory))
        .expect("create empty hardware inventory");
    }
    let guard = set_system_root_for_tests(root.path());
    Self {
      _guard: guard,
      root,
      previous_io: FILE_BEHAVIORS.take(),
    }
  }

  pub(crate) fn path(&self, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.starts_with(self.root.path()) {
      return path.to_owned();
    }
    self
      .root
      .path()
      .join(path.strip_prefix("/").unwrap_or(path))
  }

  pub(crate) fn write(&self, path: impl AsRef<Path>, value: impl AsRef<[u8]>) {
    let path = self.path(path);
    fs::create_dir_all(path.parent().expect("fixture path has a parent"))
      .unwrap();
    fs::write(path, value).unwrap();
  }

  pub(crate) fn enforce_bounds(
    &self,
    minimum: impl AsRef<Path>,
    maximum: impl AsRef<Path>,
  ) {
    let minimum = fs::canonicalize(self.path(minimum)).unwrap();
    let maximum = fs::canonicalize(self.path(maximum)).unwrap();
    self.configure(&minimum, |behavior| {
      behavior.bound = Some((maximum.clone(), Ordering::Greater));
    });
    self.configure(&maximum, |behavior| {
      behavior.bound = Some((minimum, Ordering::Less));
    });
  }

  pub(crate) fn write_count(&self, path: impl AsRef<Path>) -> usize {
    let path = fs::canonicalize(self.path(path)).unwrap();
    FILE_BEHAVIORS.with_borrow(|behaviors| {
      behaviors.get(&path).map_or(0, |behavior| behavior.writes)
    })
  }

  fn configure(
    &self,
    path: impl AsRef<Path>,
    update: impl FnOnce(&mut FileBehavior),
  ) {
    let path = fs::canonicalize(self.path(path)).unwrap();
    FILE_BEHAVIORS
      .with_borrow_mut(|behaviors| update(behaviors.entry(path).or_default()));
  }

  pub(crate) fn fail_read(&self, path: impl AsRef<Path>, error: io::ErrorKind) {
    self.configure(path, |behavior| behavior.read_error = Some(error));
  }

  /// Fails the first read after a tracked write attempt, then resumes reads.
  pub(crate) fn fail_read_after_write(
    &self,
    path: impl AsRef<Path>,
    error: io::ErrorKind,
  ) {
    self.configure(path, |behavior| {
      behavior.read_error_after_write = Some(error);
    });
  }

  pub(crate) fn write_result(
    &self,
    path: impl AsRef<Path>,
    result: Result<&str, io::ErrorKind>,
  ) {
    self.configure(path, |behavior| {
      behavior.write_result = Some(result.map(str::to_owned));
    });
  }

  pub(crate) fn clear_io(&self, path: impl AsRef<Path>) {
    let path = fs::canonicalize(self.path(path)).unwrap();
    FILE_BEHAVIORS.with_borrow_mut(|behaviors| {
      behaviors.remove(&path);
    });
  }

  /// Creates a shared cpufreq policy and the logical CPUs that refer to it.
  pub(crate) fn policy(&self, id: u32, cpus: &[u32], driver: &str) -> PathBuf {
    let path = self.path(format!("/sys/devices/system/cpu/cpufreq/policy{id}"));
    for (name, value) in [
      ("scaling_driver", driver),
      ("scaling_governor", "powersave"),
      (
        "scaling_available_governors",
        "powersave performance schedutil",
      ),
      ("scaling_cur_freq", "1500000"),
      ("cpuinfo_min_freq", "800000"),
      ("cpuinfo_max_freq", "3000000"),
      ("scaling_min_freq", "800000"),
      ("scaling_max_freq", "3000000"),
    ] {
      self.write(path.join(name), value);
    }
    let related = cpus
      .iter()
      .map(u32::to_string)
      .collect::<Vec<_>>()
      .join(" ");
    self.write(path.join("related_cpus"), &related);
    self.write(path.join("affected_cpus"), &related);
    if matches!(driver, "intel_pstate" | "amd-pstate-epp") {
      self.write(
        path.join("energy_performance_preference"),
        "balance_performance",
      );
      self.write(
        path.join("energy_performance_available_preferences"),
        "performance balance_performance balance_power power",
      );
    }
    for cpu in cpus {
      let cpu_path = self.path(format!("/sys/devices/system/cpu/cpu{cpu}"));
      fs::create_dir_all(&cpu_path).unwrap();
      symlink(&path, cpu_path.join("cpufreq")).unwrap();
    }
    path
  }

  pub(crate) fn cpu_telemetry(&self, cpus: &[u32]) {
    let mut stat = String::from("cpu 1 0 1 8 0 0 0 0\n");
    let mut info = String::new();
    for cpu in cpus {
      writeln!(stat, "cpu{cpu} 1 0 1 8 0 0 0 0").unwrap();
      writeln!(info, "processor : {cpu}\n").unwrap();
    }
    self.write("/proc/stat", stat);
    self.write("/proc/cpuinfo", info);
  }
}

impl Drop for SystemFixture {
  fn drop(&mut self) {
    FILE_BEHAVIORS.replace(mem::take(&mut self.previous_io));
  }
}

pub(crate) fn read_raw(path: impl AsRef<Path>) -> io::Result<String> {
  let path = path.as_ref();
  let error = fs::canonicalize(path).ok().and_then(|path| {
    FILE_BEHAVIORS.with_borrow_mut(|behaviors| {
      behaviors.get_mut(&path).and_then(|behavior| {
        behavior.read_error.or_else(|| {
          (behavior.writes > 0)
            .then(|| behavior.read_error_after_write.take())
            .flatten()
        })
      })
    })
  });
  if let Some(error) = error {
    return Err(error.into());
  }
  fs::read_to_string(path)
}

pub(crate) fn write_raw(
  path: impl AsRef<Path>,
  value: impl AsRef<[u8]>,
) -> io::Result<()> {
  let path = path.as_ref();
  check_bound(path, value.as_ref())?;
  let result = fs::canonicalize(path).ok().and_then(|path| {
    FILE_BEHAVIORS.with_borrow_mut(|behaviors| {
      let behavior = behaviors.entry(path).or_default();
      behavior.writes += 1;
      behavior.write_result.clone()
    })
  });
  match result {
    Some(Ok(observed)) => fs::write(path, observed),
    Some(Err(error)) => Err(error.into()),
    None => fs::write(path, value),
  }
}

fn check_bound(path: &Path, value: &[u8]) -> io::Result<()> {
  let bound = fs::canonicalize(path).ok().and_then(|path| {
    FILE_BEHAVIORS.with_borrow(|behaviors| {
      behaviors
        .get(&path)
        .and_then(|behavior| behavior.bound.clone())
    })
  });
  let Some((peer, forbidden)) = bound else {
    return Ok(());
  };
  let parse = |value: &str| {
    value
      .trim()
      .parse::<u64>()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
  };
  let value = parse(
    str::from_utf8(value)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
  )?;
  let peer = parse(&fs::read_to_string(peer)?)?;
  if value.cmp(&peer) == forbidden {
    return Err(io::ErrorKind::InvalidInput.into());
  }
  Ok(())
}
