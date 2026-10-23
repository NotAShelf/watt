use std::{
  fs as stdfs,
  io::ErrorKind,
  os::unix::fs::symlink,
  time::{
    Duration,
    Instant,
  },
};

use super::*;
use crate::{
  profile::PowerProfile,
  test_support::SystemFixture,
};

fn cpu_system(fixture: &SystemFixture, numbers: &[u32]) -> System {
  fixture.cpu_telemetry(numbers);
  System {
    cpus: cpu::Cpu::all().unwrap().into_iter().map(Arc::new).collect(),
    ..System::default()
  }
}

fn apply_rules(
  system: &System,
  rules: &[config::Rule],
  failures: &mut RuntimeFailures,
) -> Vec<fs::AppliedSetting> {
  let now = Instant::now();
  let eval = system.eval_state(now, now, PowerProfile::Balanced, false);
  failures.begin_iteration();
  let (actions, _) = evaluate_rules(rules, &eval, failures);
  apply_actions(actions, failures, &mut cpu::DmaLatency::default());
  failures.finish_iteration();
  fs::applied_settings()
}

fn setting<'a>(
  settings: &'a [fs::AppliedSetting],
  path: &Path,
) -> &'a fs::AppliedSetting {
  settings
    .iter()
    .find(|setting| setting.path == path)
    .expect("managed setting")
}

#[test]
fn identical_values_still_report_the_actual_winning_rule_per_field() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0], "intel_pstate");
  let system = cpu_system(&fixture, &[0]);
  let _managed = fs::manage_settings();
  let low: config::Rule = toml::from_str(
    "name='low'\npriority=1\ncpu.governor='performance'\ncpu.\
     energy-performance-preference='power'",
  )
  .unwrap();
  let high: config::Rule =
    toml::from_str("name='high'\npriority=9\ncpu.governor='performance'")
      .unwrap();
  let settings =
    apply_rules(&system, &[low, high], &mut RuntimeFailures::default());
  assert_eq!(
    setting(&settings, &policy.join("scaling_governor")).rule,
    Some(fs::RuleOrigin {
      name:     "high".into(),
      priority: 9,
    })
  );
  assert_eq!(
    setting(&settings, &policy.join("energy_performance_preference")).rule,
    Some(fs::RuleOrigin {
      name:     "low".into(),
      priority: 1,
    })
  );
}

#[test]
fn shared_policy_priority_does_not_steal_per_cpu_attribution() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0, 1], "intel_pstate");
  for number in [0, 1] {
    fixture.write(
      format!(
        "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
      ),
      "0",
    );
  }
  let system = cpu_system(&fixture, &[0, 1]);
  let _managed = fs::manage_settings();
  let low: config::Rule = toml::from_str(
    "name='low'\npriority=1\ncpu.governor='powersave'\ncpu.\
     pm-qos-resume-latency-us=50",
  )
  .unwrap();
  let high: config::Rule = toml::from_str(
    "name='high'\npriority=9\ncpu.for=[1]\ncpu.governor='performance'\ncpu.\
     pm-qos-resume-latency-us=100",
  )
  .unwrap();
  let settings =
    apply_rules(&system, &[low, high], &mut RuntimeFailures::default());
  assert_eq!(
    setting(&settings, &policy.join("scaling_governor"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "high"
  );
  let local = |number| {
    fixture.path(format!(
      "/sys/devices/system/cpu/cpu{number}/power/pm_qos_resume_latency_us"
    ))
  };
  assert_eq!(
    setting(&settings, &local(0)).rule.as_ref().unwrap().name,
    "low"
  );
  assert_eq!(
    setting(&settings, &local(1)).rule.as_ref().unwrap().name,
    "high"
  );
}

#[test]
fn shared_policy_uses_ascending_cpu_expression_precedence() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0, 1], "intel_pstate");
  fixture.write(policy.join("scaling_governor"), "schedutil");
  let mut system = cpu_system(&fixture, &[0, 1]);
  system.cpus = system
    .cpus
    .into_iter()
    .map(|cpu| {
      let mut cpu = (*cpu).clone();
      if cpu.number == 0 {
        cpu.available_governors = vec!["powersave".into()];
      }
      Arc::new(cpu)
    })
    .collect();
  let _managed = fs::manage_settings();
  let rule: config::Rule = toml::from_str(
    "name='ordered'\npriority=4\ncpu.for=[1,0]\ncpu.\
     governor={first-available-governor=['performance','powersave']}",
  )
  .unwrap();
  let settings = apply_rules(&system, &[rule], &mut RuntimeFailures::default());
  let governor = setting(&settings, &policy.join("scaling_governor"));
  assert_eq!(governor.requested, "powersave");
  assert_eq!(governor.rule.as_ref().unwrap().name, "ordered");
  assert_eq!(fixture.write_count(policy.join("scaling_governor")), 1);
}

#[test]
fn failed_rule_evaluation_cannot_claim_lower_rule_application() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0], "intel_pstate");
  let system = cpu_system(&fixture, &[0]);
  let _managed = fs::manage_settings();
  let low: config::Rule =
    toml::from_str("name='low'\npriority=1\ncpu.governor='performance'")
      .unwrap();
  let high: config::Rule = toml::from_str(
    "name='invalid'\npriority=9\ncpu.governor='performance'\naudio.\
     timeout-seconds='invalid'",
  )
  .unwrap();
  let mut failures = RuntimeFailures::default();
  let settings = apply_rules(&system, &[low, high], &mut failures);
  assert_eq!(failures.errors.len(), 1);
  assert_eq!(
    setting(&settings, &policy.join("scaling_governor"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "low"
  );
}

#[test]
fn successful_write_with_failed_readback_keeps_requested_rule_unverified() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0], "intel_pstate");
  let system = cpu_system(&fixture, &[0]);
  let path = policy.join("scaling_governor");
  fixture.fail_read_after_write(&path, ErrorKind::PermissionDenied);
  let _managed = fs::manage_settings();
  let rule: config::Rule =
    toml::from_str("name='owner'\npriority=8\ncpu.governor='performance'")
      .unwrap();
  let mut failures = RuntimeFailures::default();
  let settings = apply_rules(&system, &[rule], &mut failures);
  assert_eq!(failures.errors.len(), 1);
  let governor = setting(&settings, &path);
  assert_eq!(governor.requested, "performance");
  assert_eq!(governor.rule.as_ref().unwrap().name, "owner");
  assert_eq!(governor.verification(), "unverified");
}

#[test]
fn unchanged_owned_values_change_attribution_without_a_write() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0], "intel_pstate");
  let system = cpu_system(&fixture, &[0]);
  let path = policy.join("scaling_governor");
  let _managed = fs::manage_settings();
  let mut rule: config::Rule =
    toml::from_str("name='original'\npriority=1\ncpu.governor='performance'")
      .unwrap();
  apply_rules(&system, &[rule.clone()], &mut RuntimeFailures::default());
  rule.name = "replacement".into();
  rule.priority = 2;
  let settings = apply_rules(&system, &[rule], &mut RuntimeFailures::default());
  assert_eq!(
    setting(&settings, &path).rule.as_ref().unwrap().name,
    "replacement"
  );
  assert_eq!(fixture.write_count(path), 1);
}

#[test]
fn cadence_diagnostics_explain_idle_smoothing_and_bounds() {
  let now = Instant::now();
  let system = System {
    cpu_log: VecDeque::from([
      CpuLog {
        at:           now - Duration::from_secs(1),
        usage:        0.01,
        temperature:  Some(40.0),
        load_average: 0.0,
      },
      CpuLog {
        at:           now,
        usage:        0.01,
        temperature:  None,
        load_average: 0.0,
      },
    ]),
    ..System::default()
  };
  let diagnostics = PollingDiagnostics::calculate(
    &system,
    Some(Duration::from_secs(60)),
    now - Duration::from_secs(3600),
    now,
  );
  assert_eq!(diagnostics.base_interval_seconds, 5.0);
  assert_eq!(diagnostics.idle_factor, 5.0);
  assert_eq!(diagnostics.volatility_factor, 1.0);
  assert_eq!(diagnostics.requested_delay_seconds, 25.0);
  assert_eq!(diagnostics.smoothed_delay_seconds, 49.5);
  assert_eq!(diagnostics.clamped_delay_seconds, 30.0);
  assert_eq!(diagnostics.cpu_sample_count, 2);
  assert_eq!(diagnostics.temperature_sample_count, 1);
  assert!(diagnostics.cpu_usage_volatility_available);
  assert!(!diagnostics.cpu_temperature_volatility_available);
  assert!(!diagnostics.power_discharge_rate_available);
  assert_eq!(diagnostics.volatility_required_samples, 2);
  assert_eq!(diagnostics.power_discharge_required_samples, 2);
}

#[test]
fn duration_diagnostics_match_single_sample_evaluator_and_bound_requirements() {
  let now = Instant::now();
  let system = System {
    cpu_log: VecDeque::from([CpuLog {
      at:           now - Duration::from_secs(10),
      usage:        0.5,
      temperature:  None,
      load_average: 1.0,
    }]),
    ..System::default()
  };
  let config = r#"
[[rule]]
name='samples'
priority=1
if={value={cpu-usage-since='30sec'},is-more-than=0}
cpu.turbo={value={load-average-since='1sec'},is-more-than=0}
"#;
  let requirements = diagnostics::duration_requirements(config);
  let mut diagnostics = PollingDiagnostics::default();
  diagnostics.update_requirements(&requirements, &system, now);
  let cpu = diagnostics
    .duration_metrics
    .iter()
    .find(|metric| metric.metric == "cpu-usage-since")
    .unwrap();
  assert!(cpu.available);
  assert_eq!(cpu.sample_count, 1);
  assert_eq!(cpu.rule, "samples");
  assert_eq!(cpu.priority, 1);
  let load = diagnostics
    .duration_metrics
    .iter()
    .find(|metric| metric.metric == "load-average-since")
    .unwrap();
  assert!(!load.available);
  let rules: String = (1..=100)
    .map(|number| {
      format!(
        r#"[[rule]]
name='r{number}'
priority={number}
if={{value={{cpu-usage-since='{number}sec'}},is-more-than=0}}
"#
      )
    })
    .collect();
  let bounded = diagnostics::duration_requirements(&rules);
  assert_eq!(bounded.metrics.len(), 32);
  assert_eq!(bounded.omitted_count, 68);
}

#[test]
fn snapshots_remain_readable_after_hardware_disappears_without_filesystem_io() {
  let fixture = SystemFixture::new();
  let policy = fixture.policy(0, &[0], "intel_pstate");
  let mut system = cpu_system(&fixture, &[0]);
  system.device_capabilities =
    capabilities::snapshot(&system, &mut RuntimeFailures::default());
  let mut state = DaemonState::new(String::new(), 0);
  state.update_system(&system, Vec::new(), None, &RuntimeFailures::default());
  let before = state.device_capabilities().to_vec();
  stdfs::remove_dir_all(policy).unwrap();
  assert_eq!(state.device_capabilities(), before);
  assert_eq!(state.cpu_capabilities()[0].minimum_khz, Some(800_000));
  assert_eq!(state.cpu_capabilities()[0].maximum_khz, Some(3_000_000));
}

#[tokio::test]
async fn identical_reload_outcomes_each_complete_and_publish() {
  let (results, mut receiver) = broadcast::channel(64);
  for _ in 0..2 {
    let (response, result) = oneshot::channel();
    publish_reload_result(&results, Ok(()), Some(response));
    result.await.unwrap().unwrap();
  }
  // Both identical outcomes stay queued even when the subscriber is busy.
  assert_eq!(
    receiver.recv().await.unwrap(),
    (true, "configuration reloaded".into())
  );
  assert_eq!(
    receiver.recv().await.unwrap(),
    (true, "configuration reloaded".into())
  );
  publish_reload_result(
    &results,
    Err(anyhow::anyhow!("invalid candidate")),
    None,
  );
  assert_eq!(
    receiver.recv().await.unwrap(),
    (false, "invalid candidate".into())
  );
}

fn shared_radeon_system(fixture: &SystemFixture) -> (System, PathBuf) {
  let root = fixture.path("/sys/devices/shared-radeon");
  fixture.write(root.join("power_method"), "profile");
  fixture.write(root.join("power_profile"), "default");
  for name in ["card0", "card1"] {
    let card = fixture.path(format!("/sys/class/drm/{name}"));
    stdfs::create_dir_all(&card).unwrap();
    symlink(&root, card.join("device")).unwrap();
  }
  let mut system = System::default();
  system.scan_gpus().unwrap();
  (system, root)
}

#[test]
fn conflicting_radeon_aliases_apply_only_the_highest_priority_actual_owner() {
  let fixture = SystemFixture::new();
  let (system, root) = shared_radeon_system(&fixture);
  let _managed = fs::manage_settings();
  let low: config::Rule = toml::from_str(
    "name='card0-low'\npriority=1\ngpu.for=['card0']\ngpu.radeon-powersave='\
     low'",
  )
  .unwrap();
  let high: config::Rule = toml::from_str(
    "name='card1-high'\npriority=9\ngpu.for=['card1']\ngpu.radeon-powersave='\
     dynpm'",
  )
  .unwrap();
  let mut failures = RuntimeFailures::default();
  let settings = apply_rules(&system, &[low.clone(), high], &mut failures);
  assert!(failures.errors.is_empty());
  let method = setting(&settings, &root.join("power_method"));
  assert_eq!(method.requested, "dynpm");
  assert_eq!(method.rule.as_ref().unwrap().name, "card1-high");
  assert_eq!(fixture.write_count(root.join("power_method")), 1);
  assert_eq!(fixture.write_count(root.join("power_profile")), 0);
  let settings = apply_rules(&system, &[low], &mut failures);
  assert_eq!(
    setting(&settings, &root.join("power_method")).requested,
    "profile"
  );
  assert_eq!(
    setting(&settings, &root.join("power_profile"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "card0-low"
  );
}

#[test]
fn equal_priority_radeon_aliases_use_stable_ascending_alias_precedence() {
  let fixture = SystemFixture::new();
  let (system, root) = shared_radeon_system(&fixture);
  let _managed = fs::manage_settings();
  let first: config::Rule = toml::from_str(
    "name='first'\npriority=3\ngpu.for=['card0']\ngpu.radeon-powersave='low'",
  )
  .unwrap();
  let last: config::Rule = toml::from_str(
    "name='last'\npriority=3\ngpu.for=['card1']\ngpu.radeon-powersave='dynpm'",
  )
  .unwrap();
  let settings =
    apply_rules(&system, &[first, last], &mut RuntimeFailures::default());
  assert_eq!(
    setting(&settings, &root.join("power_method")).requested,
    "profile"
  );
  assert_eq!(
    setting(&settings, &root.join("power_method"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "first"
  );
  assert_eq!(fixture.write_count(root.join("power_method")), 1);
}

#[test]
fn repeated_partial_inventory_keeps_original_until_rediscovery() {
  let fixture = SystemFixture::new();
  let root = fixture.path("/sys/block/sda");
  fixture.write(root.join("queue/scheduler"), "mq-deadline");
  fixture.write(root.join("removable"), "0");
  let mut system = System::default();
  system.scan_disks().unwrap();
  let _managed = fs::manage_settings();
  let rule: config::Rule =
    toml::from_str("name='disk-owner'\npriority=5\ndisk.scheduler='none'")
      .unwrap();
  apply_rules(&system, &[rule], &mut RuntimeFailures::default());
  fixture.fail_read(root.join("removable"), ErrorKind::PermissionDenied);
  for _ in 0..2 {
    assert!(system.scan_disks().is_err());
    apply_rules(&system, &[], &mut RuntimeFailures::default());
    assert_eq!(
      stdfs::read_to_string(root.join("queue/scheduler")).unwrap(),
      "none"
    );
    assert_eq!(
      fs::applied_settings()[0].rule.as_ref().unwrap().name,
      "disk-owner"
    );
  }
  fixture.clear_io(root.join("removable"));
  system.scan_disks().unwrap();
  apply_rules(&system, &[], &mut RuntimeFailures::default());
  assert_eq!(
    stdfs::read_to_string(root.join("queue/scheduler")).unwrap(),
    "mq-deadline"
  );
  assert!(fs::applied_settings().is_empty());
}

#[test]
fn globals_do_not_attribute_an_unrelated_same_named_file() {
  let fixture = SystemFixture::new();
  let path = fixture.path("/proc/sys/vm/dirty_bytes");
  let unrelated = fixture.path("/sys/devices/unrelated/dirty_bytes");
  fixture.write(&path, "0");
  fixture.write(&unrelated, "0");
  let _managed = fs::manage_settings();
  let rule: config::Rule =
    toml::from_str("name='vm-owner'\npriority=2\nvm.dirty-bytes=100").unwrap();
  let settings =
    apply_rules(&System::default(), &[rule], &mut RuntimeFailures::default());
  assert_eq!(
    setting(&settings, &path).rule.as_ref().unwrap().name,
    "vm-owner"
  );
  fs::write(&unrelated, "100").unwrap();
  assert!(setting(&fs::applied_settings(), &unrelated).rule.is_none());
}

#[test]
fn maximum_valid_duration_uses_exact_window_for_all_retained_samples() {
  let now = Instant::now();
  let system = System {
    cpu_log: (1..=100)
      .rev()
      .map(|seconds| {
        CpuLog {
          at:           now - Duration::from_secs(seconds),
          usage:        0.5,
          temperature:  None,
          load_average: 1.0,
        }
      })
      .collect(),
    ..System::default()
  };
  let requirements = diagnostics::duration_requirements(
    r#"
[[rule]]
name='huge-window'
priority=6
if={value={cpu-usage-since='18446744073709551615sec'},is-more-than=0}
"#,
  );
  let mut snapshot = PollingDiagnostics::default();
  snapshot.update_requirements(&requirements, &system, now);
  assert_eq!(snapshot.duration_metrics.len(), 1);
  assert_eq!(snapshot.duration_metrics[0].sample_count, 100);
  assert!(snapshot.duration_metrics[0].available);
}

#[test]
fn constant_duration_branches_resolve_but_dynamic_windows_are_explicit() {
  let now = Instant::now();
  let system = System {
    cpu_log: VecDeque::from([CpuLog {
      at:           now - Duration::from_secs(10),
      usage:        0.5,
      temperature:  None,
      load_average: 1.0,
    }]),
    ..System::default()
  };
  let requirements = diagnostics::duration_requirements(
    r#"
[[rule]]
name='constant'
priority=1
if={value={cpu-usage-since={if=true,then='30sec',else='1sec'}},is-more-than=0}
[[rule]]
name='dynamic'
priority=2
if={value={cpu-usage-since={if='?discharging',then='30sec',else='1sec'}},is-more-than=0}
"#,
  );
  let mut snapshot = PollingDiagnostics::default();
  snapshot.update_requirements(&requirements, &system, now);
  let constant = snapshot
    .duration_metrics
    .iter()
    .find(|metric| metric.rule == "constant")
    .unwrap();
  assert_eq!(constant.window_seconds, Some(30.0));
  assert!(constant.available);
  let dynamic = snapshot
    .duration_metrics
    .iter()
    .find(|metric| metric.rule == "dynamic")
    .unwrap();
  assert_eq!(dynamic.window_seconds, None);
  assert!(!dynamic.available);
}

#[test]
fn shared_uncore_aliases_combine_winning_bounds_before_transition() {
  let fixture = SystemFixture::new();
  let root = fixture.path("/sys/devices/shared-uncore");
  for (file, value) in [
    ("initial_min_freq_khz", "100000"),
    ("initial_max_freq_khz", "400000"),
    ("min_freq_khz", "100000"),
    ("max_freq_khz", "200000"),
  ] {
    fixture.write(root.join(file), value);
  }
  let aliases = fixture.path("/sys/devices/system/cpu/intel_uncore_frequency");
  stdfs::create_dir_all(&aliases).unwrap();
  for name in ["alias0", "alias1"] {
    symlink(&root, aliases.join(name)).unwrap();
  }
  fixture.enforce_bounds(root.join("min_freq_khz"), root.join("max_freq_khz"));
  let mut system = System::default();
  system.scan_uncores().unwrap();
  let _managed = fs::manage_settings();
  let low: config::Rule = toml::from_str(
    "name='minimum'\npriority=1\nuncore.for=['alias0']\nuncore.\
     frequency-khz-minimum=250000",
  )
  .unwrap();
  let high: config::Rule = toml::from_str(
    "name='maximum'\npriority=9\nuncore.for=['alias1']\nuncore.\
     frequency-khz-maximum=300000",
  )
  .unwrap();
  let mut failures = RuntimeFailures::default();
  let settings = apply_rules(&system, &[low, high], &mut failures);
  assert!(failures.errors.is_empty());
  assert_eq!(
    setting(&settings, &root.join("min_freq_khz")).requested,
    "250000"
  );
  assert_eq!(
    setting(&settings, &root.join("min_freq_khz"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "minimum"
  );
  assert_eq!(
    setting(&settings, &root.join("max_freq_khz")).requested,
    "300000"
  );
  assert_eq!(
    setting(&settings, &root.join("max_freq_khz"))
      .rule
      .as_ref()
      .unwrap()
      .name,
    "maximum"
  );
  assert_eq!(fixture.write_count(root.join("min_freq_khz")), 1);
  assert_eq!(fixture.write_count(root.join("max_freq_khz")), 1);
}

#[test]
fn thermal_fallback_clears_readings_when_sensor_inventory_disappears() {
  let fixture = SystemFixture::new();
  stdfs::create_dir_all(fixture.path("/sys/class/hwmon")).unwrap();
  let root = fixture.path("/sys/devices/virtual/thermal/thermal_zone0");
  fixture.write(root.join("type"), "x86_pkg_temp");
  fixture.write(root.join("temp"), "53300");
  let mut system = System::default();
  system.scan_temperatures().unwrap();
  assert_eq!(
    system
      .cpu_temperatures
      .values()
      .copied()
      .collect::<Vec<_>>(),
    [53.3]
  );
  stdfs::remove_dir_all(fixture.path("/sys/devices/virtual/thermal")).unwrap();
  system.scan_temperatures().unwrap();
  assert!(system.cpu_temperatures.is_empty());
}
