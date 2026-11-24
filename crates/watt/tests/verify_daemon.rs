mod support;

use dbus::{
  channel::MatchingReceiver as _,
  message::MatchRule,
};
use dbus_crossroads::Crossroads;
use support::{
  Connection,
  Harness,
};

fn verifier(connection: &Connection) {
  let mut crossroads = Crossroads::new();
  let interface = crossroads.register("dev.notashelf.Watt", |builder| {
    builder.method(
      "VerifyConfiguration",
      ("contents", "hardware"),
      ("diagnostics",),
      |_, _, (_contents, hardware): (String, bool)| {
        Ok((vec![(
          if hardware { "error" } else { "warning" }.to_owned(),
          "battery".to_owned(),
          42_u16,
          "power.charge-threshold-end".to_owned(),
          "fixture diagnostic".to_owned(),
        )],))
      },
    );
  });
  crossroads.insert("/dev/notashelf/Watt", &[interface], ());
  connection.inner().start_receive(
    MatchRule::new_method_call(),
    Box::new(move |message, connection| {
      crossroads.handle_message(message, connection).unwrap();
      true
    }),
  );
}

#[tokio::test]
async fn daemon_verification_sets_exit_status() {
  let mut daemon = Harness::isolated("configuration from local file");
  daemon.start_bus().await;
  let server = Connection::address(&daemon.address).unwrap();
  verifier(&server);
  server
    .inner()
    .request_name("dev.notashelf.Watt", false, false, true)
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
  server
    .inner()
    .release_name("dev.notashelf.Watt")
    .await
    .unwrap();

  daemon.config(&" ".repeat(1024 * 1024 + 1));
  let oversized = daemon.cli(&["config", "verify", config]).await;
  assert!(!oversized.status.success());
  assert!(String::from_utf8_lossy(&oversized.stderr).contains("exceeds 1 MiB"));
  assert!(!daemon.path("/run/watt/lock").exists());
}
