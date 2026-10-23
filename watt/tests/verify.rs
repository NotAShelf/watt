mod support;

use support::{
  Harness,
  assert_success,
};

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
