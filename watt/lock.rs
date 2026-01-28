use std::{
  error::Error,
  fmt,
  fs::{
    self,
    File,
    OpenOptions,
  },
  io::Write,
  ops,
  path::{
    Path,
    PathBuf,
  },
  process,
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
  pub path: PathBuf,
  pid:      u32,
}

impl fmt::Display for LockFileError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.pid == 0 {
      write!(f, "failed to acquire lock at {}", self.path.display())
    } else {
      write!(f, "another watt daemon is running (PID: {})", self.pid)
    }
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

  pub fn acquire(
    lock_path: &Path,
    force: bool,
  ) -> Result<Option<Self>, LockFileError> {
    let pid = process::id();

    #[allow(clippy::suspicious_open_options)]
    let file = OpenOptions::new()
      .create(true)
      .read(true)
      .write(true)
      .open(lock_path)
      .map_err(|error| {
        log::error!(
          "failed to open lock file at {}: {}",
          lock_path.display(),
          error
        );
        LockFileError {
          path: lock_path.to_owned(),
          pid:  0,
        }
      })?;

    let mut lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
      Ok(lock) => lock,
      Err((_, nix::errno::Errno::EWOULDBLOCK)) => {
        let Some(existing_pid) = Self::read_pid(lock_path) else {
          if force {
            log::warn!(
              "could not determine PID of existing watt instance, starting \
               anyway",
            );
            return Ok(None);
          }

          return Err(LockFileError {
            path: lock_path.to_owned(),
            pid:  0,
          });
        };

        if force {
          log::warn!(
            "another watt instance is running (PID: {existing_pid}), starting \
             anyway",
          );
          return Ok(None);
        }

        return Err(LockFileError {
          path: lock_path.to_owned(),
          pid:  existing_pid,
        });
      },

      Err((_, error)) => {
        log::error!("failed to acquire lock: {}", error);
        return Err(LockFileError {
          path: lock_path.to_owned(),
          pid:  0,
        });
      },
    };

    if let Err(e) = lock.set_len(0) {
      log::error!("failed to truncate lock file: {}", e);
      return Err(LockFileError {
        path: lock_path.to_owned(),
        pid:  0,
      });
    }

    if let Err(e) = lock.write_all(format!("{pid}\n").as_bytes()) {
      log::error!("failed to write PID to lock file: {}", e);
      return Err(LockFileError {
        path: lock_path.to_owned(),
        pid:  0,
      });
    }

    if let Err(e) = lock.sync_all() {
      log::error!("failed to sync lock file: {}", e);
      return Err(LockFileError {
        path: lock_path.to_owned(),
        pid:  0,
      });
    }

    Ok(Some(LockFile {
      lock,
      path: lock_path.to_owned(),
    }))
  }

  fn read_pid(lock_path: &Path) -> Option<u32> {
    match fs::read_to_string(lock_path) {
      Ok(content) => content.trim().parse().ok(),
      Err(_) => None,
    }
  }

  pub fn release(&mut self) {
    let _ = fs::remove_file(&self.path);
  }
}

impl Drop for LockFile {
  fn drop(&mut self) {
    self.release();
  }
}
