use std::{
  env,
  fs::{
    self,
    File,
    OpenOptions,
  },
  io::{
    self,
    Read as _,
    Seek as _,
    SeekFrom,
  },
  path::Path,
  process::{
    Child,
    Command,
    Output,
    Stdio,
  },
};

/// Own a child and file-backed output so polling never waits for pipe EOF.
pub struct Process {
  pub child: Child,
  stdout:    File,
  stderr:    File,
}

impl Process {
  pub fn capture(command: &mut Command) -> io::Result<Self> {
    Self::spawn(command, tempfile::tempfile()?, tempfile::tempfile()?)
  }

  pub fn logged(
    command: &mut Command,
    stdout: &Path,
    stderr: &Path,
  ) -> io::Result<Self> {
    Self::spawn(command, capture_file(stdout)?, capture_file(stderr)?)
  }

  fn spawn(
    command: &mut Command,
    stdout: File,
    stderr: File,
  ) -> io::Result<Self> {
    let child = command
      .stdin(Stdio::null())
      .stdout(stdout.try_clone()?)
      .stderr(stderr.try_clone()?)
      .spawn()?;
    Ok(Self {
      child,
      stdout,
      stderr,
    })
  }

  pub fn try_output(&mut self) -> io::Result<Option<Output>> {
    let Some(status) = self.child.try_wait()? else {
      return Ok(None);
    };
    Ok(Some(Output {
      status,
      stdout: read_capture(&mut self.stdout)?,
      stderr: read_capture(&mut self.stderr)?,
    }))
  }
}

impl Drop for Process {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

fn capture_file(path: &Path) -> io::Result<File> {
  OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(true)
    .open(path)
}

fn read_capture(file: &mut File) -> io::Result<Vec<u8>> {
  file.seek(SeekFrom::Start(0))?;
  let mut bytes = Vec::new();
  file.read_to_end(&mut bytes)?;
  Ok(bytes)
}

/// Keep an accidental daemon invocation away from host hardware and state.
pub fn isolated_command(executable: &str, root: &Path) -> Command {
  for directory in ["runtime", "home/config", "state", "run/watt"] {
    fs::create_dir_all(root.join(directory)).unwrap();
  }
  let address = format!("unix:path={}/bus", root.display());
  let mut command = Command::new(executable);
  command
    .env_clear()
    .env(
      "PATH",
      env::var_os("PATH").expect("PATH is required for process tests"),
    )
    .env("LANG", "C")
    .env("HOME", root.join("home"))
    .env("TMPDIR", root.join("runtime"))
    .env("XDG_RUNTIME_DIR", root.join("runtime"))
    .env("XDG_CONFIG_HOME", root.join("home/config"))
    .env("XDG_STATE_HOME", root.join("state"))
    .env("WATT_SYSTEM_ROOT", root)
    .env("TOKIO_WORKER_THREADS", "2")
    .env("DBUS_SYSTEM_BUS_ADDRESS", &address)
    .env("DBUS_SESSION_BUS_ADDRESS", &address)
    .stdin(Stdio::null());
  command
}
