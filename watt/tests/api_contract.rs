mod support;

use std::sync::atomic::Ordering;

use futures_util::StreamExt as _;
use support::{
  Dictionary,
  GOVERNOR,
  Harness,
  assert_denied,
  bounded,
  call,
  native,
  ppd,
  reload,
  reloaded,
  rule,
  string,
  value,
};
use zbus::{
  Connection,
  Proxy,
  connection,
};

fn attribute<'a>(xml: &'a str, name: &str) -> &'a str {
  xml
    .split_once(&format!("{name}=\""))
    .unwrap()
    .1
    .split('"')
    .next()
    .unwrap()
}

fn interface<'a>(xml: &'a str, name: &str) -> &'a str {
  xml
    .split_once(&format!("<interface name=\"{name}\">"))
    .unwrap_or_else(|| panic!("missing interface {name}: {xml}"))
    .1
    .split("</interface>")
    .next()
    .unwrap()
}

fn member<'a>(xml: &'a str, kind: &str, name: &str) -> &'a str {
  let start = xml
    .find(&format!("<{kind} name=\"{name}\""))
    .unwrap_or_else(|| panic!("missing {kind} {name}: {xml}"));
  let xml = &xml[start..];
  let header_end = xml.find('>').unwrap() + 1;
  if xml[..header_end].ends_with("/>") {
    return &xml[..header_end];
  }
  let end = xml.find(&format!("</{kind}>")).unwrap();
  &xml[..end]
}

fn assert_signature(
  xml: &str,
  kind: &str,
  name: &str,
  input: &str,
  output: &str,
) {
  let member = member(xml, kind, name);
  let arguments: Vec<_> = member.split("<arg ").skip(1).collect();
  let inputs: String = arguments
    .iter()
    .filter(|arg| arg.contains("direction=\"in\""))
    .map(|arg| attribute(arg, "type"))
    .collect();
  let outputs: String = arguments
    .iter()
    .filter(|arg| !arg.contains("direction=\"in\""))
    .map(|arg| attribute(arg, "type"))
    .collect();
  assert_eq!(inputs, input, "{kind} {name} input changed");
  assert_eq!(outputs, output, "{kind} {name} output changed");
}

fn assert_property(xml: &str, name: &str, signature: &str, access: &str) {
  let property = member(xml, "property", name);
  assert_eq!(attribute(property, "type"), signature, "property {name}");
  assert_eq!(attribute(property, "access"), access, "property {name}");
}

async fn introspect(
  connection: &Connection,
  destination: &str,
  path: &str,
) -> String {
  let proxy = Proxy::new(
    connection,
    destination,
    path,
    "org.freedesktop.DBus.Introspectable",
  )
  .await
  .unwrap();
  bounded(proxy.call("Introspect", &())).await.unwrap()
}

#[tokio::test]
async fn public_bus_names_paths_and_signatures_remain_additive() {
  let mut daemon = Harness::start("rule = []\n").await;
  let connection = daemon.connection();
  let xml =
    introspect(&connection, "dev.notashelf.Watt", "/dev/notashelf/Watt").await;
  let watt = interface(&xml, "dev.notashelf.Watt");
  for (name, input, output) in [
    ("Reload", "", ""),
    ("VerifyConfiguration", "sb", "a(ssqss)"),
    ("GetStatus", "", "a{sv}"),
    ("GetCpuPolicies", "", "a(ssaus)"),
    ("GetAppliedRules", "", "as"),
    ("GetRules", "", "a(sq)"),
    ("GetCpuCapabilities", "", "aa{sv}"),
    ("GetBoostCapabilities", "", "a{sv}"),
    ("GetRecentErrors", "", "as"),
    ("GetAppliedSettings", "", "a(ssss)"),
    ("GetConfig", "", "s"),
    ("GetAppliedSettingDetails", "", "aa{sv}"),
    ("GetPollingDiagnostics", "", "a{sv}"),
    ("GetDeviceCapabilities", "", "aa{sv}"),
  ] {
    assert_signature(watt, "method", name, input, output);
  }
  for (name, signature) in [
    ("Version", "s"),
    ("RuleCount", "u"),
    ("CpuCount", "u"),
    ("PerformanceDegraded", "s"),
    ("ErrorCount", "u"),
    ("DegradedConditions", "aa{sv}"),
    ("LatestError", "s"),
    ("InterfaceVersion", "u"),
  ] {
    assert_property(watt, name, signature, "read");
  }
  for (name, signature) in [
    ("AppliedRulesChanged", ""),
    ("AppliedSettingsChanged", ""),
    ("ConfigurationReloaded", "bs"),
    ("CapabilitiesChanged", ""),
  ] {
    assert_signature(watt, "signal", name, "", signature);
  }
  let proxy = native(&connection).await;
  assert_eq!(
    bounded(proxy.get_property::<u32>("InterfaceVersion"))
      .await
      .unwrap(),
    1
  );

  let xml = introspect(
    &connection,
    "net.hadess.PowerProfiles",
    "/net/hadess/PowerProfiles",
  )
  .await;
  let ppd = interface(&xml, "net.hadess.PowerProfiles");
  assert_property(ppd, "ActiveProfile", "s", "readwrite");
  assert_property(ppd, "Profiles", "aa{sv}", "read");
  assert_signature(ppd, "method", "HoldProfile", "sss", "u");
  assert_signature(ppd, "method", "ReleaseProfile", "u", "");
  assert_signature(ppd, "signal", "ProfileReleased", "", "u");
  daemon.shutdown("-TERM").await;
}

async fn profile(native: &Proxy<'_>) -> String {
  let status: Dictionary = call(native, "GetStatus").await;
  string(&status, "profile").to_owned()
}

#[tokio::test]
async fn mock_authorization_denies_mutations_without_changing_live_state() {
  let mut daemon =
    Harness::start(&rule("initial", "cpu.governor = 'performance'")).await;
  let connection = daemon.connection();
  let native = native(&connection).await;
  let ppd = ppd(&connection).await;
  let hold = ("power-saver", "test", "watt-process-test");
  daemon.wait_file(GOVERNOR, "performance").await;
  let before: String = call(&native, "GetConfig").await;
  daemon.config(&rule("replacement", "cpu.governor = 'schedutil'"));

  daemon.allowed.store(false, Ordering::SeqCst);
  assert_denied(
    bounded(native.call::<_, _, ()>("Reload", &()))
      .await
      .unwrap_err(),
  );
  assert_eq!(call::<String>(&native, "GetConfig").await, before);
  assert_eq!(daemon.read(GOVERNOR), "performance");
  assert_denied(
    bounded(ppd.set_property("ActiveProfile", "performance"))
      .await
      .unwrap_err(),
  );
  assert_eq!(profile(&native).await, "balanced");
  assert_denied(
    bounded(ppd.call::<_, _, u32>("HoldProfile", &hold))
      .await
      .unwrap_err(),
  );

  daemon.allowed.store(true, Ordering::SeqCst);
  reload(&native).await;
  daemon.wait_file(GOVERNOR, "schedutil").await;
  bounded(ppd.set_property("ActiveProfile", "performance"))
    .await
    .unwrap();
  assert_eq!(profile(&native).await, "performance");
  let cookie: u32 = bounded(ppd.call("HoldProfile", &hold)).await.unwrap();
  assert_eq!(profile(&native).await, "power-saver");

  let other = bounded(
    connection::Builder::address(daemon.address.as_str())
      .unwrap()
      .build(),
  )
  .await
  .unwrap();
  assert_denied(
    bounded(
      support::ppd(&other)
        .await
        .call::<_, _, ()>("ReleaseProfile", &(cookie,)),
    )
    .await
    .unwrap_err(),
  );
  assert_eq!(profile(&native).await, "power-saver");
  bounded(ppd.call::<_, _, ()>("ReleaseProfile", &(cookie,)))
    .await
    .unwrap();
  assert_eq!(profile(&native).await, "performance");
  // The owner releasing its own hold must not consult polkit.
  assert_eq!(daemon.actions.load(Ordering::SeqCst), 6);
  daemon.shutdown("-TERM").await;
}

#[tokio::test]
async fn applied_setting_details_preserve_legacy_results_and_rule_attribution()
{
  let mut daemon =
    Harness::start(&rule("attributed", "cpu.governor = 'performance'")).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(GOVERNOR, "performance").await;
  let path = daemon.path(GOVERNOR).display().to_string();
  let legacy: Vec<(String, String, String, String)> =
    call(&proxy, "GetAppliedSettings").await;
  assert_eq!(legacy, [(
    path.clone(),
    "performance".to_owned(),
    "performance".to_owned(),
    "verified".to_owned()
  )]);
  let details: Vec<Dictionary> = call(&proxy, "GetAppliedSettingDetails").await;
  let [detail] = details.as_slice() else {
    panic!("expected one setting: {details:?}");
  };
  assert_eq!(string(detail, "path"), path);
  assert_eq!(string(detail, "requested"), "performance");
  assert_eq!(string(detail, "observed"), "performance");
  assert_eq!(string(detail, "verification"), "verified");
  assert_eq!(string(detail, "rule"), "attributed");
  assert_eq!(value::<u16>(detail, "priority"), 10);

  let capabilities: Vec<Dictionary> = call(&proxy, "GetCpuCapabilities").await;
  let [cpu] = capabilities.as_slice() else {
    panic!("expected one CPU: {capabilities:?}");
  };
  assert_eq!(value::<u64>(cpu, "min-khz"), 800_000);
  assert_eq!(value::<u64>(cpu, "max-khz"), 3_000_000);
  daemon.shutdown("-TERM").await;
}

#[tokio::test]
async fn native_signals_announce_reload_rule_setting_and_capability_changes() {
  let mut daemon =
    Harness::start(&rule("initial", "cpu.governor = 'performance'")).await;
  let connection = daemon.connection();
  let proxy = native(&connection).await;
  daemon.wait_file(GOVERNOR, "performance").await;
  let mut reloads =
    proxy.receive_signal("ConfigurationReloaded").await.unwrap();
  let mut rules = proxy.receive_signal("AppliedRulesChanged").await.unwrap();
  let mut settings = proxy
    .receive_signal("AppliedSettingsChanged")
    .await
    .unwrap();
  let mut capabilities =
    proxy.receive_signal("CapabilitiesChanged").await.unwrap();

  daemon.config(&rule("updated", "cpu.governor = 'schedutil'"));
  reload(&proxy).await;
  let (success, message) = reloaded(&mut reloads).await;
  assert!(success, "{message}");
  bounded(rules.next()).await.unwrap();
  bounded(settings.next()).await.unwrap();
  daemon.wait_file(GOVERNOR, "schedutil").await;
  assert_eq!(call::<Vec<String>>(&proxy, "GetAppliedRules").await, [
    "updated"
  ]);

  daemon.policy(1);
  daemon.telemetry(&[0, 1]);
  daemon.signal("-HUP");
  let (success, message) = reloaded(&mut reloads).await;
  assert!(success, "{message}");
  bounded(capabilities.next()).await.unwrap();
  let devices: Vec<Dictionary> = call(&proxy, "GetDeviceCapabilities").await;
  let root = daemon.path("/sys/devices/system/cpu/cpufreq");
  let policies: Vec<_> = devices
    .iter()
    .filter(|device| string(device, "subsystem") == "cpu-policy")
    .collect();
  assert_eq!(policies.len(), 2);
  for policy in policies {
    let path = string(policy, "path");
    assert!(
      path.starts_with(root.to_str().unwrap()),
      "capability escaped private root: {path}"
    );
    let controls: Vec<String> = value(policy, "controls");
    for control in ["governor", "epp", "frequency-minimum", "frequency-maximum"]
    {
      assert!(controls.iter().any(|name| name == control), "{controls:?}");
    }
  }
  assert_eq!(
    call::<Vec<Dictionary>>(&proxy, "GetCpuCapabilities")
      .await
      .len(),
    2
  );
  let diagnostics: Dictionary = call(&proxy, "GetPollingDiagnostics").await;
  assert!(
    (1.0..=30.0).contains(&value::<f64>(&diagnostics, "clamped-delay-seconds"))
  );
  assert!(value::<f64>(&diagnostics, "evaluation-elapsed-seconds") >= 0.0);
  assert!(value::<u64>(&diagnostics, "cpu-sample-count") > 0);
  daemon.shutdown("-TERM").await;
}
