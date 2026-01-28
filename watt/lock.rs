use std::{
  error::Error,
  fmt,
  fs::{
    File,
    OpenOptions,
  },
  ops,
  os::unix::fs::OpenOptionsExt,
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
    write!(f, "failed to acquire lock on {}", self.path.display())?;
    if let Some(msg) = &self.message {
      write!(f, ": {}", msg)?;
    }
    Ok(())
  }
}

impl Error for LockFileError {}

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
    #[allow(clippy::suspicious_open_options)]
    let file = OpenOptions::new()
      .create(true)
      .read(true)
      .write(true)
      .mode(0o600)
      .open(lock_path)
      .map_err(|error| {
        log::error!(
          "failed to open lock file at {}: {}",
          lock_path.display(),
          error
        );
        LockFileError {
          path:    lock_path.to_owned(),
          message: Some(error.to_string()),
        }
      })?;

    let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(
      |(_, error)| {
        let message = if error == nix::errno::Errno::EWOULDBLOCK {
          log::error!(
            "another watt instance is already running (lock held on {})",
            lock_path.display()
          );
          Some("another instance is running".to_string())
        } else {
          log::error!("failed to acquire lock: {}", error);
          Some(error.to_string())
        };

        LockFileError {
          path: lock_path.to_owned(),
          message,
        }
      },
    )?;

    Ok(LockFile {
      lock,
      path: lock_path.to_owned(),
    })
  }
}
