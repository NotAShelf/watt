mod support;

use support::{
  Harness,
  bounded,
};
use zbus::{
  connection,
  interface,
};

struct Verifier;

#[interface(name = "dev.notashelf.Watt")]
impl Verifier {
  fn verify_configuration(
    &self,
    _contents: &str,
    hardware: bool,
  ) -> Vec<(String, String, u16, String, String)> {
    vec![(
      if hardware { "error" } else { "warning" }.into(),
      "battery".into(),
      42,
      "power.charge-threshold-end".into(),
      "fixture diagnostic".into(),
    )]
  }
}

#[tokio::test]
async fn daemon_verification_sets_exit_status() {
  let mut daemon = Harness::isolated("configuration from local file");
  daemon.start_bus().await;
  let server = bounded(
    connection::Builder::address(daemon.address.as_str())
      .unwrap()
      .name("dev.notashelf.Watt")
      .unwrap()
      .serve_at("/dev/notashelf/Watt", Verifier)
      .unwrap()
      .build(),
  )
  .await
  .unwrap();
  let config = daemon.path("/config.toml");
  let config = config.to_str().unwrap();
  let warnings = daemon.cli(&["config", "verify", config]).await;
  assert!(warnings.status.success());
  let errors = daemon
    .cli(&["config", "verify", config, "--hardware"])
    .await;
  assert!(!errors.status.success());
  bounded(server.close()).await.unwrap();

  daemon.config(&" ".repeat(1024 * 1024 + 1));
  let oversized = daemon.cli(&["config", "verify", config]).await;
  assert!(!oversized.status.success());
  assert!(String::from_utf8_lossy(&oversized.stderr).contains("exceeds 1 MiB"));
  assert!(!daemon.path("/run/watt/lock").exists());
}
