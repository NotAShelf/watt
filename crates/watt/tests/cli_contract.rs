mod support;

use std::{
  fs,
  process::Output,
  sync::atomic::Ordering,
};

use serde_json::{
  Value,
  json,
};
use support::{
  DIRTY_RATIO,
  GOVERNOR,
  Harness,
  assert_success,
  rule,
};

async fn query(daemon: &Harness, arguments: &[&str]) -> Value {
  let output = daemon.cli(arguments).await;
  assert_success(&output);
  assert_eq!(
    output.stdout.last(),
    Some(&b'\n'),
    "JSON output needs a record delimiter"
  );
  serde_json::from_slice(&output.stdout)
    .expect("CLI did not return one JSON record")
}

fn denied(output: Output) {
  assert!(!output.status.success(), "denied mutation reported success");
  assert!(String::from_utf8_lossy(&output.stderr).contains("AccessDenied"));
}

#[tokio::test]
async fn json_queries_expose_live_values_without_variant_wrappers() {
  let mut daemon = Harness::start(&rule(
    "json-rule",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 15",
  ))
  .await;
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.wait_file(DIRTY_RATIO, "15").await;
  let status = query(&daemon, &["config", "--json", "status"]).await;
  assert_eq!(status["profile"], "balanced");
  assert_eq!(status["is-discharging"], false);
  assert!(status["polling-delay-ms"].as_u64().unwrap() >= 1_000);
  let rules = query(&daemon, &["config", "--json", "rules"]).await;
  assert_eq!(rules, json!([{"name": "json-rule", "priority": 10}]));
  let active = query(&daemon, &["config", "--json", "active"]).await;
  assert_eq!(active, json!(["json-rule"]));
  let capabilities =
    query(&daemon, &["config", "--json", "capabilities"]).await;
  let cpu = &capabilities["cpus"][0];
  assert_eq!(cpu["cpu"], 0);
  assert_eq!(cpu["min-khz"], 800_000);
  assert_eq!(cpu["max-khz"], 3_000_000);
  assert_eq!(
    cpu["governors"],
    json!(["powersave", "performance", "schedutil"])
  );
  let policy_path = daemon
    .path("/sys/devices/system/cpu/cpufreq/policy0")
    .display()
    .to_string();
  let policy = capabilities["devices"]
    .as_array()
    .unwrap()
    .iter()
    .find(|device| device["path"] == policy_path)
    .unwrap();
  assert_eq!(policy["subsystem"], "cpu-policy");
  assert!(
    policy["controls"]
      .as_array()
      .unwrap()
      .contains(&json!("governor"))
  );

  let settings = query(&daemon, &["config", "--json", "settings"]).await;
  let governor_path = daemon.path(GOVERNOR).display().to_string();
  let governor = settings
    .as_array()
    .unwrap()
    .iter()
    .find(|setting| setting["path"] == governor_path)
    .unwrap();
  assert_eq!(
    governor,
    &json!({
      "path": governor_path, "requested": "performance", "observed": "performance",
      "verification": "verified", "rule": "json-rule", "priority": 10,
      "cpus": [0],
    })
  );
  let polling = query(&daemon, &["config", "--json", "polling"]).await;
  assert!(
    (1.0..=30.0).contains(&polling["clamped-delay-seconds"].as_f64().unwrap())
  );
  assert!(polling["cpu-sample-count"].as_u64().unwrap() > 0);
  assert!(polling["evaluation-elapsed-seconds"].as_f64().unwrap() >= 0.0);
  let profile = query(&daemon, &["config", "--json", "profile", "get"]).await;
  assert_eq!(profile, json!({"profile": "balanced"}));

  fs::remove_file(daemon.path(DIRTY_RATIO)).unwrap();
  fs::create_dir(daemon.path(DIRTY_RATIO)).unwrap();
  daemon.config(&rule(
    "failed-vm",
    "cpu.governor = 'performance'\nvm.dirty-ratio = 25",
  ));
  let output = daemon.cli(&["config", "reload"]).await;
  assert!(
    output.status.success(),
    "config loading should not fail merely because an action fails"
  );
  let errors = query(&daemon, &["config", "--json", "errors"]).await;
  assert!(
    errors
      .as_array()
      .unwrap()
      .iter()
      .any(|error| error.as_str().unwrap().contains("dirty_ratio")),
    "failed VM action was not exposed: {errors}"
  );
  daemon.shutdown("-TERM").await;
  assert_eq!(daemon.read(GOVERNOR), "powersave");
}

#[tokio::test]
async fn cli_mutations_report_denial_and_change_state_only_when_authorized() {
  let mut daemon =
    Harness::start(&rule("initial", "cpu.governor = 'performance'")).await;
  daemon.wait_file(GOVERNOR, "performance").await;
  daemon.config(&rule("authorized", "cpu.governor = 'schedutil'"));
  daemon.allowed.store(false, Ordering::SeqCst);
  denied(daemon.cli(&["config", "reload"]).await);
  assert_eq!(daemon.read(GOVERNOR), "performance");
  assert_eq!(
    query(&daemon, &["config", "--json", "rules"]).await,
    json!([{"name": "initial", "priority": 10}])
  );
  denied(
    daemon
      .cli(&["config", "profile", "set", "performance"])
      .await,
  );
  assert_eq!(
    query(&daemon, &["config", "--json", "profile", "get"]).await,
    json!({"profile": "balanced"})
  );

  daemon.allowed.store(true, Ordering::SeqCst);
  assert_success(&daemon.cli(&["config", "reload"]).await);
  daemon.wait_file(GOVERNOR, "schedutil").await;
  let output = daemon
    .cli(&["config", "profile", "set", "performance"])
    .await;
  assert_success(&output);
  assert_eq!(
    query(&daemon, &["config", "--json", "status"]).await["profile"],
    "performance"
  );
  assert_eq!(
    query(&daemon, &["config", "--json", "profile", "get"]).await,
    json!({"profile": "performance"})
  );
  daemon.shutdown("-TERM").await;
}

#[tokio::test]
async fn absent_bus_fails_queries_and_json_mutations_are_rejected_before_connecting()
 {
  let mut daemon = Harness::isolated("rule = []\n");
  daemon.address =
    format!("unix:path={}", daemon.path("/missing-bus").display());
  for command in [
    "status",
    "capabilities",
    "rules",
    "errors",
    "settings",
    "polling",
    "active",
  ] {
    let output = daemon.cli(&["config", "--json", command]).await;
    assert!(
      !output.status.success(),
      "absent daemon returned success for {command}"
    );
    assert!(
      output.stdout.is_empty(),
      "query returned output without a running daemon: {command}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("D-Bus"));
  }
  let output = daemon.cli(&["config", "--json", "profile", "get"]).await;
  assert!(!output.status.success());
  assert!(output.stdout.is_empty());
  for arguments in [
    vec!["config", "--json", "reload"],
    vec!["config", "--json", "profile", "set", "performance"],
    vec!["config", "--json", "schema"],
    vec!["config", "--json", "show", "--default"],
    vec!["config", "--json", "verify", "/missing-config"],
  ] {
    let output = daemon.cli(&arguments).await;
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
      error.contains("--json is supported only for daemon queries"),
      "wrong validation layer: {error}"
    );
    assert!(
      !error.contains("failed to connect"),
      "unsupported mode reached the bus: {error}"
    );
  }
}
