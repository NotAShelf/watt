#![allow(dead_code)]

mod process;

use std::{
  fs,
  future::Future,
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
use zbus::connection;

pub const DEADLINE: Duration = Duration::from_secs(15);

pub async fn bounded<T>(future: impl Future<Output = T>) -> T {
  timeout(DEADLINE, future)
    .await
    .expect("process scenario timed out")
}

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

/// Bus process with temporary runtime files.
/// Drop the child guard before deleting its files.
pub struct Harness {
  bus:         Option<Process>,
  pub address: String,
  root:        TempDir,
}

impl Harness {
  pub fn isolated(config: &str) -> Self {
    let root = tempfile::tempdir().unwrap();
    let harness = Self {
      bus: None,
      address: format!("unix:path={}/bus", root.path().display()),
      root,
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

  fn command(&self, executable: &str) -> Command {
    let mut command = isolated_command(executable, self.root.path());
    command
      .env("DBUS_SYSTEM_BUS_ADDRESS", &self.address)
      .env("DBUS_SESSION_BUS_ADDRESS", &self.address);
    command
  }

  /// Start a private bus and wait until it accepts connections.
  pub async fn start_bus(&mut self) {
    let address = self.address.clone();
    let bus = self.bus.insert(
      Process::logged(
        self
          .command("dbus-daemon")
          .args(["--session", "--nofork"])
          .arg(format!("--address={address}")),
        &self.path("/bus.stdout"),
        &self.path("/bus.log"),
      )
      .expect("dbus-daemon is required for process tests"),
    );
    bounded(async {
      let mut ticks = interval(Duration::from_millis(10));
      loop {
        ticks.tick().await;
        assert!(
          bus.child.try_wait().unwrap().is_none(),
          "private bus exited"
        );
        if connection::Builder::address(address.as_str())
          .unwrap()
          .build()
          .await
          .is_ok()
        {
          break;
        }
      }
    })
    .await;
  }

  pub async fn cli(&self, arguments: &[&str]) -> Output {
    run(self.command(env!("CARGO_BIN_EXE_watt")).args(arguments)).await
  }
}
