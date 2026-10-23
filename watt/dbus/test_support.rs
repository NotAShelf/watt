use std::{
  collections::HashMap,
  io::{
    BufRead as _,
    BufReader,
  },
  process::{
    Child,
    Command,
    Stdio,
  },
};

use serde::Serialize;
use zbus::zvariant::Type;

#[derive(Serialize, Type)]
pub(super) struct AuthorizationResult {
  pub authorized: bool,
  pub challenge:  bool,
  pub details:    HashMap<String, String>,
}

pub(crate) struct TestBus {
  child:       Child,
  pub address: String,
  _runtime:    tempfile::TempDir,
}

impl TestBus {
  pub fn new() -> Self {
    let runtime = tempfile::tempdir().unwrap();
    let child = Command::new("dbus-daemon")
      .args(["--session", "--nofork", "--print-address=1"])
      .env("XDG_RUNTIME_DIR", runtime.path())
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon is required for D-Bus tests");
    let mut bus = Self {
      child,
      address: String::new(),
      _runtime: runtime,
    };
    BufReader::new(bus.child.stdout.take().unwrap())
      .read_line(&mut bus.address)
      .unwrap();
    bus.address = bus.address.trim().to_owned();
    assert!(!bus.address.is_empty(), "dbus-daemon did not start");
    bus
  }
}

impl Drop for TestBus {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}
