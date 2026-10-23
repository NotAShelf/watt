mod support;

use std::{
  fs,
  io::Write as _,
};

use futures_util::{
  StreamExt as _,
  stream::FuturesUnordered,
};
use support::{
  DIRTY_RATIO,
  EPP,
  GOVERNOR,
  Harness,
  bounded,
  call,
  native,
  reload,
  reloaded,
  rule,
};
use zbus::Error;

fn reload_accepted(result: Result<(), Error>) -> bool {
  match result {
    Ok(()) => true,
    Err(Error::MethodError(name, ..)) => {
      assert_eq!(name.as_str(), "org.freedesktop.DBus.Error.LimitsExceeded");
      false
    },
    Err(error) => panic!("unexpected concurrent reload failure: {error}"),
  }
}

#[tokio::test]
async fn termination_and_interrupt_restore_original_controls() {
  for signal in ["-TERM", "-INT"] {
    let mut daemon = Harness::start(&rule(
      "tuning",
      "cpu.governor = 'performance'\ncpu.energy-performance-preference = \
       'power'\nvm.dirty-ratio = 15",
    ))
    .await;
    daemon.wait_file(GOVERNOR, "performance").await;
    daemon.wait_file(EPP, "power").await;
    daemon.wait_file(DIRTY_RATIO, "15").await;
    daemon.shutdown(signal).await;
    assert_eq!(daemon.read(GOVERNOR), "powersave");
    assert_eq!(daemon.read(EPP), "balance_performance");
    assert_eq!(daemon.read(DIRTY_RATIO), "60");
  }
}

#[tokio::test]
async fn reload_adds_changes_and_removes_actions_without_losing_originals() {
  let mut daemon =
    Harness::start(&rule("initial", "cpu.governor = 'performance'")).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(GOVERNOR, "performance").await;

  daemon.config(&rule(
    "added",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 20",
  ));
  reload(&proxy).await;
  daemon.wait_file(DIRTY_RATIO, "20").await;
  daemon.config(&rule(
    "changed",
    "cpu.governor = 'schedutil'\nvm.dirty-ratio = 30",
  ));
  reload(&proxy).await;
  daemon.wait_file(GOVERNOR, "schedutil").await;
  daemon.wait_file(DIRTY_RATIO, "30").await;

  daemon.config(&rule("removed", "cpu.governor = 'schedutil'"));
  reload(&proxy).await;
  daemon.wait_file(DIRTY_RATIO, "60").await;
  daemon.config("rule = []\n");
  reload(&proxy).await;
  daemon.wait_file(GOVERNOR, "powersave").await;
  let settings: Vec<(String, String, String, String)> =
    call(&proxy, "GetAppliedSettings").await;
  assert!(
    settings.is_empty(),
    "removed rules left owned settings: {settings:?}"
  );
  daemon.shutdown("-TERM").await;
  assert_eq!(daemon.read(DIRTY_RATIO), "60");
}

#[tokio::test]
async fn invalid_reload_retains_loaded_config_and_reports_both_reload_routes() {
  let mut daemon =
    Harness::start(&rule("retained", "cpu.governor = 'performance'")).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(GOVERNOR, "performance").await;
  let original: String = call(&proxy, "GetConfig").await;
  let mut signals =
    proxy.receive_signal("ConfigurationReloaded").await.unwrap();

  daemon.config("[[rule]\nthis is not TOML\n");
  let error = bounded(proxy.call::<_, _, ()>("Reload", &()))
    .await
    .unwrap_err();
  assert!(error.to_string().contains("reload failed"), "{error}");
  assert!(!reloaded(&mut signals).await.0);
  assert_eq!(call::<String>(&proxy, "GetConfig").await, original);
  assert_eq!(daemon.read(GOVERNOR), "performance");

  daemon.signal("-HUP");
  assert!(!reloaded(&mut signals).await.0);
  assert_eq!(call::<String>(&proxy, "GetConfig").await, original);

  daemon.config(&rule("recovered", "cpu.governor = 'schedutil'"));
  reload(&proxy).await;
  let (success, message) = reloaded(&mut signals).await;
  assert!(success, "{message}");
  daemon.wait_file(GOVERNOR, "schedutil").await;
  daemon.shutdown("-TERM").await;
  assert_eq!(daemon.read(GOVERNOR), "powersave");
}

#[tokio::test]
async fn shutdown_preserves_external_modifications() {
  let mut daemon = Harness::start(&rule(
    "owned",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 15",
  ))
  .await;
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.wait_file(DIRTY_RATIO, "15").await;
  daemon.write(GOVERNOR, "schedutil\n");
  daemon.shutdown("-TERM").await;
  assert_eq!(daemon.read(GOVERNOR), "schedutil");
  assert_eq!(daemon.read(DIRTY_RATIO), "60");
}

#[tokio::test]
async fn one_restoration_failure_does_not_prevent_independent_restoration() {
  let mut daemon = Harness::start(&rule(
    "partial",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 15",
  ))
  .await;
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.wait_file(DIRTY_RATIO, "15").await;
  // Reading a directory fails even when the test runs as root.
  fs::remove_file(daemon.path(DIRTY_RATIO)).unwrap();
  fs::create_dir(daemon.path(DIRTY_RATIO)).unwrap();
  let log = daemon.shutdown("-TERM").await;
  assert!(
    log.contains("dirty_ratio"),
    "restoration failure was not reported: {log}"
  );
  assert_eq!(daemon.read(GOVERNOR), "powersave");
  assert!(daemon.path(DIRTY_RATIO).is_dir());
}

#[tokio::test]
async fn hotplug_reload_and_device_disappearance_at_shutdown_restore_survivors()
{
  let config = rule(
    "hotplug",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 15",
  );
  let mut daemon = Harness::start(&config).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.wait_file(DIRTY_RATIO, "15").await;
  daemon.fifo("/config.toml");
  let reload_connection = connection.clone();
  let request = tokio::spawn(async move {
    native(&reload_connection)
      .await
      .call::<_, _, ()>("Reload", &())
      .await
  });
  let mut writer = daemon.fifo_reader("/config.toml").await;
  daemon.policy(1);
  daemon.telemetry(&[0, 1]);
  assert_eq!(
    daemon.read("/sys/devices/system/cpu/cpufreq/policy1/scaling_governor"),
    "powersave"
  );
  writer.write_all(config.as_bytes()).unwrap();
  drop(writer);
  bounded(request).await.unwrap().unwrap();
  daemon.config(&config);
  let added = "/sys/devices/system/cpu/cpufreq/policy1/scaling_governor";
  daemon.wait_file(added, "performance").await;
  assert_eq!(
    bounded(proxy.get_property::<u32>("CpuCount"))
      .await
      .unwrap(),
    2
  );

  daemon.remove_policy(0);
  daemon.telemetry(&[1]);
  reload(&proxy).await;
  assert_eq!(
    bounded(proxy.get_property::<u32>("CpuCount"))
      .await
      .unwrap(),
    1
  );
  // Prepare a tuned device that will disappear while restoration is blocked.
  daemon.policy(2);
  daemon.telemetry(&[1, 2]);
  reload(&proxy).await;
  daemon
    .wait_file(
      "/sys/devices/system/cpu/cpufreq/policy2/scaling_governor",
      "performance",
    )
    .await;
  daemon.fifo(DIRTY_RATIO);
  daemon.signal("-TERM");
  let mut writer = daemon.fifo_reader(DIRTY_RATIO).await;
  assert_eq!(
    daemon.read(added),
    "performance",
    "independent restoration did not pause behind the FIFO"
  );
  daemon.remove_policy(2);
  // An external value lets restoration preserve this FIFO without writing it.
  writer.write_all(b"99\n").unwrap();
  drop(writer);
  daemon.wait_exit().await;
  assert_eq!(daemon.read(added), "powersave");
}

#[tokio::test]
async fn repeated_hangups_and_concurrent_native_reloads_remain_bounded() {
  let mut daemon = Harness::start(&rule("first", "vm.dirty-ratio = 10")).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(DIRTY_RATIO, "10").await;
  let mut signals =
    proxy.receive_signal("ConfigurationReloaded").await.unwrap();
  for value in 11..19 {
    daemon.config(&rule(
      &format!("reload-{value}"),
      &format!("vm.dirty-ratio = {value}"),
    ));
    daemon.signal("-HUP");
    let (success, message) = reloaded(&mut signals).await;
    assert!(success, "{message}");
    daemon.wait_file(DIRTY_RATIO, &value.to_string()).await;
  }

  let config = rule("reload-18", "vm.dirty-ratio = 18");
  daemon.fifo("/config.toml");
  let reload_connection = connection.clone();
  let first_request = tokio::spawn(async move {
    native(&reload_connection)
      .await
      .call::<_, _, ()>("Reload", &())
      .await
  });
  let mut writer = daemon.fifo_reader("/config.toml").await;
  let mut requests = FuturesUnordered::new();
  for _ in 0..32 {
    let proxy = proxy.clone();
    requests.push(async move { proxy.call::<_, _, ()>("Reload", &()).await });
  }
  // One queued request can wait; the rest must fail promptly while loading
  // blocks.
  assert!(!reload_accepted(bounded(requests.next()).await.unwrap()));
  daemon.config(&config);
  writer.write_all(config.as_bytes()).unwrap();
  drop(writer);
  bounded(first_request).await.unwrap().unwrap();
  let successes = bounded(async {
    let mut successes = 0;
    while let Some(result) = requests.next().await {
      successes += usize::from(reload_accepted(result));
    }
    successes
  })
  .await;
  assert!(successes > 0);
  daemon.assert_running();
  assert_eq!(daemon.read(DIRTY_RATIO), "18");
  daemon.shutdown("-TERM").await;
  assert_eq!(daemon.read(DIRTY_RATIO), "60");
}

#[tokio::test]
async fn unavailable_bus_does_not_stop_tuning_reload_or_restoration() {
  let mut daemon =
    Harness::isolated(&rule("without-bus", "cpu.governor = 'performance'"));
  daemon.address =
    format!("unix:path={}", daemon.path("/missing-bus").display());
  daemon.spawn_daemon();
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.config(&rule("signal-reload", "cpu.governor = 'schedutil'"));
  daemon.signal("-HUP");
  daemon.wait_file(GOVERNOR, "schedutil").await;
  let log = daemon.shutdown("-TERM").await;
  assert!(log.contains("D-Bus"), "bus failure was not logged: {log}");
  assert_eq!(daemon.read(GOVERNOR), "powersave");
}
