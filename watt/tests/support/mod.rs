#![allow(dead_code)]

mod process;

use std::{
  fs,
  path::{
    Path,
    PathBuf,
  },
  process::{
    Command,
    Output,
  },
  time::Duration,
};

use process::Process;
pub use process::isolated_command;
use tempfile::TempDir;
use tokio::time::{
  interval,
  timeout,
};

pub const DEADLINE: Duration = Duration::from_secs(15);

/// Poll until the expected state appears or the deadline expires.
pub async fn eventually(description: &str, mut ready: impl FnMut() -> bool) {
  timeout(DEADLINE, async {
    let mut ticks = interval(Duration::from_millis(10));
    loop {
      ticks.tick().await;
      if ready() {
        return;
      }
    }
  })
  .await
  .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

pub async fn run(command: &mut Command) -> Output {
  let mut child = Process::capture(command).unwrap();
  let mut output = None;
  eventually("process exit", || {
    output = child.try_output().unwrap();
    output.is_some()
  })
  .await;
  output.unwrap()
}

pub fn assert_success(output: &Output) {
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
}

/// Temporary runtime files for invoking the CLI.
pub struct Harness {
  root: TempDir,
}

impl Harness {
  pub fn isolated(config: &str) -> Self {
    let harness = Self {
      root: tempfile::tempdir().unwrap(),
    };
    harness.config(config);
    harness
  }

  pub fn path(&self, path: impl AsRef<Path>) -> PathBuf {
    self
      .root
      .path()
      .join(path.as_ref().strip_prefix("/").unwrap())
  }

  pub fn write(&self, path: impl AsRef<Path>, value: impl AsRef<[u8]>) {
    let path = self.path(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
  }

  pub fn config(&self, contents: &str) {
    self.write("/config.toml", contents);
  }

  pub async fn cli(&self, arguments: &[&str]) -> Output {
    run(
      isolated_command(env!("CARGO_BIN_EXE_watt"), self.root.path())
        .args(arguments),
    )
    .await
  }
}
