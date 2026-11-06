#![allow(dead_code)]

mod process;

use std::{
  collections::HashMap,
  fs,
  future::Future,
  os::unix::fs::{
    OpenOptionsExt as _,
    symlink,
  },
  path::{
    Path,
    PathBuf,
  },
  pin::Pin,
  process::{
    Command,
    Output,
  },
  sync::{
    Arc,
    atomic::{
      AtomicBool,
      AtomicU32,
      Ordering,
    },
  },
  task::{
    Context,
    Poll,
  },
  time::Duration,
};

use dbus::{
  Message,
  arg::{
    AppendAll,
    Arg,
    Get,
    PropMap,
    prop_cast,
  },
  channel::{
    Channel,
    MatchingReceiver as _,
  },
  message::MatchRule,
  nonblock::{
    MsgMatch,
    Proxy,
    SyncConnection,
    stdintf::org_freedesktop_dbus::Properties as _,
  },
};
use dbus_crossroads::Crossroads;
use dbus_tokio::connection as tokio_dbus;
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::{
  Stream,
  StreamExt as _,
};
use nix::{
  libc,
  sys::stat::Mode,
  unistd::mkfifo,
};
use process::Process;
pub use process::isolated_command;
use tempfile::TempDir;
use tokio::{
  task::JoinHandle,
  time::{
    interval,
    timeout,
  },
};

pub const DEADLINE: Duration = Duration::from_secs(15);
pub const GOVERNOR: &str =
  "/sys/devices/system/cpu/cpufreq/policy0/scaling_governor";
pub const EPP: &str =
  "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference";
pub const DIRTY_RATIO: &str = "/proc/sys/vm/dirty_ratio";

pub type Dictionary = PropMap;

#[derive(Clone)]
pub struct Connection {
  inner:    Arc<SyncConnection>,
  resource: Arc<ConnectionResource>,
}

struct ConnectionResource(JoinHandle<()>);

impl Drop for ConnectionResource {
  fn drop(&mut self) {
    self.0.abort();
  }
}

impl Connection {
  pub fn address(address: &str) -> Result<Self, dbus::Error> {
    let mut channel = Channel::open_private(address)?;
    channel.register()?;
    let (resource, inner) = tokio_dbus::from_channel(channel)?;
    let resource = tokio::spawn(async move {
      let _ = resource.await;
    });
    Ok(Self {
      inner,
      resource: Arc::new(ConnectionResource(resource)),
    })
  }

  pub fn inner(&self) -> &Arc<SyncConnection> {
    &self.inner
  }
}
#[derive(Clone)]
pub struct ServiceProxy {
  connection:  Connection,
  destination: &'static str,
  path:        &'static str,
  interface:   &'static str,
}

impl ServiceProxy {
  fn proxy(&self) -> Proxy<'_, Arc<SyncConnection>> {
    Proxy::new(
      self.destination,
      self.path,
      DEADLINE,
      self.connection.inner.clone(),
    )
  }

  pub async fn call<R>(&self, method: &str) -> Result<R, dbus::Error>
  where
    R: Arg + for<'a> Get<'a> + 'static,
  {
    let (result,): (R,) =
      self.proxy().method_call(self.interface, method, ()).await?;
    Ok(result)
  }

  pub async fn invoke<A: AppendAll>(
    &self,
    method: &str,
    args: A,
  ) -> Result<(), dbus::Error> {
    self.proxy().method_call(self.interface, method, args).await
  }

  pub async fn invoke_result<A, R>(
    &self,
    method: &str,
    args: A,
  ) -> Result<R, dbus::Error>
  where
    A: AppendAll,
    R: Arg + for<'a> Get<'a> + 'static,
  {
    let (result,): (R,) = self
      .proxy()
      .method_call(self.interface, method, args)
      .await?;
    Ok(result)
  }

  pub async fn get_property<R>(&self, property: &str) -> Result<R, dbus::Error>
  where
    R: Arg + for<'a> Get<'a> + 'static,
  {
    self.proxy().get(self.interface, property).await
  }

  pub async fn set_property(
    &self,
    property: &str,
    value: &str,
  ) -> Result<(), dbus::Error> {
    self
      .proxy()
      .set(self.interface, property, value.to_owned())
      .await
  }

  pub async fn receive_signal(
    &self,
    member: &'static str,
  ) -> Result<SignalStream, dbus::Error> {
    let mut rule = MatchRule::new_signal(self.interface, member);
    rule.path = Some(self.path.into());
    let (guard, inner) =
      self.connection.inner.add_match(rule).await?.msg_stream();
    Ok(SignalStream {
      _guard: guard,
      inner,
    })
  }
}

pub struct SignalStream {
  _guard: MsgMatch,
  inner:  UnboundedReceiver<Message>,
}

impl Stream for SignalStream {
  type Item = Message;

  fn poll_next(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
  ) -> Poll<Option<Self::Item>> {
    Pin::new(&mut self.inner).poll_next(context)
  }
}

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

pub fn assert_denied(error: dbus::Error) {
  assert_eq!(
    error.name(),
    Some("org.freedesktop.DBus.Error.AccessDenied"),
    "{error}"
  );
}

pub async fn call<R>(proxy: &ServiceProxy, method: &str) -> R
where
  R: Arg + for<'a> Get<'a> + 'static,
{
  bounded(proxy.call(method)).await.unwrap()
}

pub async fn reload(proxy: &ServiceProxy) {
  bounded(proxy.invoke("Reload", ())).await.unwrap();
}

/// Next `ConfigurationReloaded` result as `(success, message)`.
pub async fn reloaded(signals: &mut SignalStream) -> (bool, String) {
  bounded(signals.next()).await.unwrap().read2().unwrap()
}

pub fn string<'a>(dictionary: &'a Dictionary, key: &str) -> &'a str {
  prop_cast::<String>(dictionary, key).unwrap()
}

pub fn value<T>(dictionary: &Dictionary, key: &str) -> T
where
  T: Clone + 'static,
{
  prop_cast::<T>(dictionary, key).unwrap().clone()
}

/// Private-bus polkit authority controlled by the test.
struct MockAuthority {
  allowed: Arc<AtomicBool>,
  actions: Arc<AtomicU32>,
}

fn bus_config(address: &str) -> String {
  format!(
    r#"<busconfig>
  <type>session</type>
  <listen>{address}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
  )
}

/// Daemon and bus processes with temporary sysfs, procfs and runtime files.
/// Drop the child guards before deleting their files.
pub struct Harness {
  daemon:      Option<Process>,
  bus:         Option<Process>,
  authority:   Option<Connection>,
  connection:  Option<Connection>,
  pub allowed: Arc<AtomicBool>,
  pub actions: Arc<AtomicU32>,
  pub address: String,
  root:        TempDir,
}

impl Harness {
  pub fn isolated(config: &str) -> Self {
    let root = tempfile::tempdir().unwrap();
    let harness = Self {
      daemon: None,
      bus: None,
      authority: None,
      connection: None,
      allowed: Arc::new(AtomicBool::new(true)),
      actions: Arc::new(AtomicU32::new(0)),
      address: format!("unix:path={}/bus", root.path().display()),
      root,
    };
    harness.base_tree();
    harness.policy(0);
    harness.telemetry(&[0]);
    harness.config(config);
    harness
  }

  pub async fn start(config: &str) -> Self {
    let mut harness = Self::isolated(config);
    harness.start_bus().await;
    harness.spawn_daemon();
    harness.wait_initial_poll().await;
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

  pub fn read(&self, path: &str) -> String {
    fs::read_to_string(self.path(path))
      .unwrap()
      .trim()
      .to_owned()
  }

  fn base_tree(&self) {
    for directory in [
      "/sys/class/hwmon",
      "/sys/class/power_supply",
      "/sys/class/drm",
      "/sys/class/input",
      "/sys/devices/virtual/thermal",
      "/sys/block",
      "/sys/bus/usb/devices",
    ] {
      fs::create_dir_all(self.path(directory)).unwrap();
    }
    self.write("/sys/class/dmi/id/chassis_type", "3\n");
    self.write("/proc/loadavg", "0.00 0.00 0.00 1/1 1\n");
    self.write(DIRTY_RATIO, "60\n");
  }

  pub fn policy(&self, number: u32) {
    let policy = format!("/sys/devices/system/cpu/cpufreq/policy{number}");
    for (name, value) in [
      ("scaling_driver", "intel_pstate"),
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
      ("energy_performance_preference", "balance_performance"),
      (
        "energy_performance_available_preferences",
        "performance balance_performance balance_power power",
      ),
    ] {
      self.write(format!("{policy}/{name}"), value);
    }
    self.write(format!("{policy}/related_cpus"), number.to_string());
    self.write(format!("{policy}/affected_cpus"), number.to_string());
    let cpu = self.path(format!("/sys/devices/system/cpu/cpu{number}"));
    fs::create_dir_all(&cpu).unwrap();
    symlink(self.path(policy), cpu.join("cpufreq")).unwrap();
  }

  pub fn remove_policy(&self, number: u32) {
    for path in [
      format!("/sys/devices/system/cpu/cpu{number}"),
      format!("/sys/devices/system/cpu/cpufreq/policy{number}"),
    ] {
      fs::remove_dir_all(self.path(path)).unwrap();
    }
  }

  pub fn telemetry(&self, cpus: &[u32]) {
    let mut stat = String::from("cpu 1 0 1 8 0 0 0 0\n");
    let mut info = String::new();
    for cpu in cpus {
      stat.push_str(&format!("cpu{cpu} 1 0 1 8 0 0 0 0\n"));
      info.push_str(&format!("processor : {cpu}\n\n"));
    }
    self.write("/proc/stat", stat);
    self.write("/proc/cpuinfo", info);
  }

  pub fn config(&self, contents: &str) {
    // Readers must see a complete config during concurrent reloads.
    self.write("/config.next", contents);
    fs::rename(self.path("/config.next"), self.path("/config.toml")).unwrap();
  }

  fn command(&self, executable: &str) -> Command {
    let mut command = isolated_command(executable, self.root.path());
    command
      .env("DBUS_SYSTEM_BUS_ADDRESS", &self.address)
      .env("DBUS_SESSION_BUS_ADDRESS", &self.address);
    command
  }

  /// Start a private bus with a mock polkit authority on it.
  pub async fn start_bus(&mut self) {
    let address = self.address.clone();
    self.write("/bus.conf", bus_config(&address));
    let bus = self.bus.insert(
      Process::logged(
        self.command("dbus-daemon").arg("--nofork").arg(format!(
          "--config-file={}",
          self.path("/bus.conf").display()
        )),
        &self.path("/bus.stdout"),
        &self.path("/bus.log"),
      )
      .expect("dbus-daemon is required for process tests"),
    );
    let client = bounded(async {
      let mut ticks = interval(Duration::from_millis(10));
      loop {
        ticks.tick().await;
        assert!(
          bus.child.try_wait().unwrap().is_none(),
          "private bus exited"
        );
        if let Ok(connection) = Connection::address(&address) {
          break connection;
        }
      }
    })
    .await;

    let authority = Connection::address(&address).unwrap();
    let mut crossroads = Crossroads::new();
    let interface =
      crossroads.register("org.freedesktop.PolicyKit1.Authority", |builder| {
        builder.method(
          "CheckAuthorization",
          ("subject", "action", "details", "flags", "cancellation_id"),
          ("result",),
          |_,
           authority: &mut MockAuthority,
           (subject, ..): (
            (String, PropMap),
            String,
            HashMap<String, String>,
            u32,
            String,
          )| {
            assert_eq!(subject.0, "system-bus-name");
            assert!(
              prop_cast::<String>(&subject.1, "name")
                .unwrap()
                .starts_with(':')
            );
            authority.actions.fetch_add(1, Ordering::SeqCst);
            Ok(((
              authority.allowed.load(Ordering::SeqCst),
              false,
              HashMap::<String, String>::new(),
            ),))
          },
        );
      });
    crossroads.insert(
      "/org/freedesktop/PolicyKit1/Authority",
      &[interface],
      MockAuthority {
        allowed: self.allowed.clone(),
        actions: self.actions.clone(),
      },
    );
    authority.inner.start_receive(
      MatchRule::new_method_call(),
      Box::new(move |message, connection| {
        crossroads.handle_message(message, connection).unwrap();
        true
      }),
    );
    authority
      .inner
      .request_name("org.freedesktop.PolicyKit1", false, false, true)
      .await
      .unwrap();
    self.authority = Some(authority);
    self.connection = Some(client);
  }

  pub fn spawn_daemon(&mut self) {
    self.daemon = Some(
      Process::logged(
        self
          .command(env!("CARGO_BIN_EXE_watt"))
          .arg("--config")
          .arg(self.path("/config.toml")),
        &self.path("/daemon.stdout"),
        &self.path("/daemon.log"),
      )
      .unwrap(),
    );
  }

  /// Wait until the daemon owns its bus name and has polled the CPUs once.
  async fn wait_initial_poll(&mut self) {
    let connection = self.connection();
    let proxy = native(&connection).await;
    bounded(async {
      let mut ticks = interval(Duration::from_millis(10));
      loop {
        ticks.tick().await;
        self.assert_running();
        if let Ok(cpus) =
          proxy.call::<Vec<Dictionary>>("GetCpuCapabilities").await
          && !cpus.is_empty()
        {
          return;
        }
      }
    })
    .await;
  }

  pub async fn cli(&self, arguments: &[&str]) -> Output {
    run(self.command(env!("CARGO_BIN_EXE_watt")).args(arguments)).await
  }

  pub fn connection(&self) -> Connection {
    self.connection.as_ref().unwrap().clone()
  }

  pub fn assert_running(&mut self) {
    assert!(
      self
        .daemon
        .as_mut()
        .unwrap()
        .child
        .try_wait()
        .unwrap()
        .is_none(),
      "daemon exited: {}",
      self.read("/daemon.log")
    );
  }

  pub async fn wait_file(&mut self, path: &str, expected: &str) {
    eventually(&format!("{path} = {expected}"), || {
      self.assert_running();
      fs::read_to_string(self.path(path))
        .is_ok_and(|value| value.trim() == expected)
    })
    .await;
  }

  pub fn fifo(&self, path: &str) {
    let path = self.path(path);
    fs::remove_file(&path).unwrap();
    mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
  }

  /// Opening a nonblocking writer succeeds only once the daemon is reading.
  pub async fn fifo_reader(&mut self, path: &str) -> fs::File {
    let mut writer = None;
    eventually("daemon attached to FIFO", || {
      self.assert_running();
      match fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(self.path(path))
      {
        Ok(file) => {
          writer = Some(file);
          true
        },
        Err(error) => {
          assert_eq!(
            error.raw_os_error(),
            Some(libc::ENXIO),
            "unexpected FIFO error: {error}"
          );
          false
        },
      }
    })
    .await;
    writer.unwrap()
  }

  pub fn signal(&self, name: &str) {
    let pid = self.daemon.as_ref().unwrap().child.id().to_string();
    assert!(
      Command::new("kill")
        .args([name, &pid])
        .status()
        .unwrap()
        .success()
    );
  }

  pub async fn shutdown(&mut self, signal: &str) -> String {
    self.signal(signal);
    self.wait_exit().await
  }

  pub async fn wait_exit(&mut self) -> String {
    let mut status = None;
    eventually("clean daemon exit", || {
      status = self.daemon.as_mut().unwrap().child.try_wait().unwrap();
      status.is_some()
    })
    .await;
    assert!(status.unwrap().success(), "{}", self.read("/daemon.log"));
    self.read("/daemon.log")
  }
}

fn proxy(connection: &Connection, name: &'static str) -> ServiceProxy {
  let path = match name {
    "dev.notashelf.Watt" => "/dev/notashelf/Watt",
    "net.hadess.PowerProfiles" => "/net/hadess/PowerProfiles",
    _ => unreachable!(),
  };
  ServiceProxy {
    connection: connection.clone(),
    destination: name,
    path,
    interface: name,
  }
}

pub async fn native(connection: &Connection) -> ServiceProxy {
  proxy(connection, "dev.notashelf.Watt")
}

pub async fn ppd(connection: &Connection) -> ServiceProxy {
  proxy(connection, "net.hadess.PowerProfiles")
}

pub fn rule(name: &str, actions: &str) -> String {
  format!("[[rule]]\nname = {name:?}\npriority = 10\n{actions}\n")
}
