use std::{
  error::Error,
  fmt,
  fs::{
    self,
    File,
    OpenOptions,
  },
  io::Write as _,
  ops,
  os::unix::fs::{
    DirBuilderExt,
    OpenOptionsExt,
  },
  path::{
    Path,
    PathBuf,
  },
};

#[cfg(unix)] use nix::fcntl::{
  Flock,
  FlockArg,
};

#[cfg(not(unix))]
compile_error!("watt is only supported on Unix-like systems");

pub struct LockFile {
  lock: Flock<File>,
  path: PathBuf,
}

#[derive(Debug)]
pub struct LockFileError {
  pub path:    PathBuf,
  pub message: Option<String>,
}

impl fmt::Display for LockFileError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "failed to acquire lock on {path}",
      path = self.path.display(),
    )?;
    if let Some(message) = &self.message {
      write!(f, ": {message}")?;
    }
    Ok(())
  }
}

impl Error for LockFileError {}

fn pid_is_alive(pid: u32) -> bool {
  fs::metadata(format!("/proc/{pid}")).is_ok()
}

fn read_lock_pid(path: &Path) -> Option<u32> {
  fs::read_to_string(path)
    .ok()
    .and_then(|s| s.trim().parse().ok())
}

fn lock_contention_message(lock_path: &Path) -> String {
  let holder = read_lock_pid(lock_path);
  let stale = holder.is_some_and(|pid| !pid_is_alive(pid));

  if stale {
    log::error!(
      "stale lock file at {path} (previous holder is dead)",
      path = lock_path.display(),
    );
    "stale lock file, previous holder no longer running".to_string()
  } else {
    log::error!(
      "another watt instance is already running (lock held on \
       {path}{pid_info})",
      path = lock_path.display(),
      pid_info = holder.map_or(String::new(), |p| format!(", pid {p}")),
    );
    holder.map_or_else(
      || "another instance is running".to_string(),
      |p| format!("another instance is running (pid {p})"),
    )
  }
}

impl ops::Deref for LockFile {
  type Target = File;

  fn deref(&self) -> &Self::Target {
    &self.lock
  }
}

impl ops::DerefMut for LockFile {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.lock
  }
}

impl LockFile {
  pub fn path(&self) -> &Path {
    &self.path
  }

  pub fn acquire(lock_path: &Path) -> Result<Self, LockFileError> {
    // Ensure parent directory exists with proper permissions
    if let Some(parent) = lock_path.parent()
      && !parent.exists()
    {
      fs::DirBuilder::new()
        .mode(0o755)
        .recursive(true)
        .create(parent)
        .map_err(|error| {
          log::error!(
            "failed to create lock directory {parent}: {error}",
            parent = parent.display(),
          );
          LockFileError {
            path:    lock_path.to_owned(),
            message: Some(format!(
              "cannot create directory {parent}: {error}",
              parent = parent.display(),
            )),
          }
        })?;
    }

    #[allow(clippy::suspicious_open_options)]
    let file = OpenOptions::new()
      .create(true)
      .read(true)
      .write(true)
      .mode(0o600)
      .open(lock_path)
      .map_err(|error| {
        log::error!(
          "failed to open lock file at {path}: {error}",
          path = lock_path.display(),
        );
        LockFileError {
          path:    lock_path.to_owned(),
          message: Some(error.to_string()),
        }
      })?;

    let mut lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(
      |(_, error)| {
        let message = if error == nix::errno::Errno::EWOULDBLOCK {
          Some(lock_contention_message(lock_path))
        } else {
          log::error!("failed to acquire lock: {error}");
          Some(error.to_string())
        };

        LockFileError {
          path: lock_path.to_owned(),
          message,
        }
      },
    )?;

    lock.set_len(0).map_err(|error| {
      log::error!(
        "failed to truncate lock file at {path}: {error}",
        path = lock_path.display(),
      );
      LockFileError {
        path:    lock_path.to_owned(),
        message: Some(error.to_string()),
      }
    })?;

    write!(lock, "{}", std::process::id()).map_err(|error| {
      log::error!(
        "failed to write PID to lock file at {path}: {error}",
        path = lock_path.display(),
      );
      LockFileError {
        path:    lock_path.to_owned(),
        message: Some(error.to_string()),
      }
    })?;

    Ok(LockFile {
      lock,
      path: lock_path.to_owned(),
    })
  }
}
