mod support;

use std::fs;

use support::{
  Harness,
  assert_success,
  isolated_command,
  run,
};

#[tokio::test]
async fn hardware_verification_reports_controls_and_rejects_partial_discovery()
{
  let directory = tempfile::tempdir().unwrap();
  let root = directory.path();
  let write = |path: &str, contents: &str| {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
  };
  for path in [
    "sys/devices/system/cpu/cpu0",
    "sys/block",
    "sys/bus/usb/devices",
    "sys/class/drm",
  ] {
    fs::create_dir_all(root.join(path)).unwrap();
  }
  write("proc/stat", "cpu 1 0 1 8 0 0 0 0\ncpu0 1 0 1 8 0 0 0 0\n");
  write("proc/cpuinfo", "processor : 0\n\n");
  write("sys/firmware/acpi/platform_profile", "balanced");
  write("sys/firmware/acpi/platform_profile_choices", "balanced");
  write("sys/class/power_supply/BAT0/type", "Battery");
  write("sys/class/power_supply/BAT1/type", "Battery");
  write(
    "sys/class/power_supply/BAT1/charge_control_start_threshold",
    "40",
  );
  write(
    "sys/class/power_supply/BAT1/charge_control_end_threshold",
    "80",
  );
  write(
    "config.toml",
    "[[rule]]\nname='hardware'\npriority=1\ncpu.governor='performance'\npower.\
     platform-profile='performance'\npower.for=['BAT0','BAT1']\npower.\
     charge-threshold-end=60\n",
  );
  let read = |path: &str| fs::read_to_string(root.join(path)).unwrap();
  let mut command = isolated_command(env!("CARGO_BIN_EXE_watt"), root);
  command
    .args(["--verify-config=hardware", "--config"])
    .arg(root.join("config.toml"));

  let output = run(&mut command).await;
  assert_success(&output);
  assert!(!root.join("run/watt/lock").exists());
  let stderr = String::from_utf8_lossy(&output.stderr);
  for expected in [
    "cpu.governor",
    "unavailable",
    "power.platform-profile",
    "unsupported",
    "power.charge-threshold-end",
  ] {
    assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
  }
  assert_eq!(
    read("sys/class/power_supply/BAT1/charge_control_end_threshold"),
    "80"
  );
  assert_eq!(read("sys/firmware/acpi/platform_profile"), "balanced");

  for (broken, error) in [
    ("sys/devices/system/cpu/cpu1", "discovery is incomplete"),
    (
      "sys/class/power_supply/broken",
      "power-supply hardware discovery is incomplete",
    ),
  ] {
    fs::create_dir_all(root.join(broken)).unwrap();
    let output = run(&mut command).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(error));
    assert!(output.stdout.is_empty());
    fs::remove_dir(root.join(broken)).unwrap();
  }
  assert_eq!(
    read("sys/class/power_supply/BAT1/charge_control_end_threshold"),
    "80"
  );
}

#[tokio::test]
async fn verification_exits_without_starting_the_daemon() {
  let daemon = Harness::isolated("[[rule]]\nname='test'\npriority=1\n");
  let config = daemon.path("/config.toml");
  let config = config.to_str().unwrap();
  assert_success(&daemon.cli(&["--verify-config", "--config", config]).await);

  daemon.config(
    "[[rule]]\nname='test'\npriority=1\ncpu.frequency-mhz-minimum=-1\n",
  );
  let invalid = daemon.cli(&["--verify-config", "--config", config]).await;
  assert!(!invalid.status.success());
  assert!(
    String::from_utf8_lossy(&invalid.stderr)
      .contains("cpu.frequency-mhz-minimum")
  );

  let conflict = daemon
    .cli(&["--verify-config", "config", "show", "--default"])
    .await;
  assert!(!conflict.status.success());
  assert!(!daemon.path("/run/watt/lock").exists());
}
