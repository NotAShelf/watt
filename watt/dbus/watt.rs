use std::{
  collections::HashMap,
  sync::Arc,
};

use tokio::sync::RwLock;
use zbus::{
  interface,
  object_server::SignalEmitter,
  zvariant::Value,
};

use crate::system::DaemonState;

pub struct WattInterface {
  state: Arc<RwLock<DaemonState>>,
}

impl WattInterface {
  pub fn new(state: Arc<RwLock<DaemonState>>) -> Self {
    Self { state }
  }
}

#[interface(name = "dev.notashelf.Watt")]
impl WattInterface {
  #[zbus(property)]
  async fn version(&self) -> String {
    env!("CARGO_PKG_VERSION").to_owned()
  }

  #[zbus(property)]
  async fn rule_count(&self) -> u32 {
    let state = self.state.read().await;
    state.rule_count() as u32
  }

  #[zbus(property)]
  async fn cpu_count(&self) -> u32 {
    let state = self.state.read().await;
    state.cpu_count() as u32
  }

  #[zbus(property)]
  async fn performance_degraded(&self) -> String {
    self
      .state
      .read()
      .await
      .performance_degraded()
      .unwrap_or_default()
      .to_owned()
  }

  #[zbus(property)]
  async fn error_count(&self) -> u32 {
    self.state.read().await.error_count() as u32
  }

  #[zbus(property)]
  async fn degraded_conditions(&self) -> Vec<HashMap<String, Value<'_>>> {
    self
      .state
      .read()
      .await
      .degraded_conditions()
      .iter()
      .map(|condition| {
        HashMap::from([
          ("context".into(), Value::from(condition.context.clone())),
          ("message".into(), Value::from(condition.message.clone())),
        ])
      })
      .collect()
  }

  #[zbus(property)]
  async fn latest_error(&self) -> String {
    self
      .state
      .read()
      .await
      .latest_error()
      .unwrap_or_default()
      .to_owned()
  }

  async fn get_status(&self) -> HashMap<String, Value<'_>> {
    let state = self.state.read().await;
    let mut status = HashMap::new();

    if let Some(log) = state.latest_cpu_log() {
      status.insert("cpu-usage".to_owned(), Value::from(log.usage * 100.0));

      if let Some(temperature) = log.temperature {
        status.insert("cpu-temperature".to_owned(), Value::from(temperature));
      }
    }

    status.insert(
      "profile".to_owned(),
      Value::from(String::from(state.active_profile().as_str())),
    );

    status.insert(
      "is-discharging".to_owned(),
      Value::from(state.is_discharging()),
    );

    status.insert(
      "error-count".to_owned(),
      Value::from(state.error_count() as u64),
    );

    status.insert(
      "performance-degraded".to_owned(),
      Value::from(state.performance_degraded().unwrap_or_default().to_owned()),
    );

    if let Some(error) = state.latest_error() {
      status.insert("latest-error".to_owned(), Value::from(error.to_owned()));
    }

    status
  }

  async fn get_cpu_policies(&self) -> Vec<(String, String, Vec<u32>, String)> {
    self
      .state
      .read()
      .await
      .cpu_policies()
      .into_iter()
      .map(|policy| {
        (
          policy.name.clone(),
          policy.path.display().to_string(),
          policy.related_cpus.clone(),
          policy.driver.clone().unwrap_or_default(),
        )
      })
      .collect()
  }

  async fn get_applied_rules(&self) -> Vec<String> {
    let state = self.state.read().await;
    state.last_applied_rules()
  }

  async fn get_cpu_capabilities(&self) -> Vec<HashMap<String, Value<'_>>> {
    let cpus = self.state.read().await.cpu_capabilities();
    cpus
      .into_iter()
      .map(|cpu| {
        HashMap::from([
          ("cpu".into(), Value::from(cpu.number)),
          ("policy".into(), Value::from(cpu.policy.unwrap_or_default())),
          (
            "frequency-control".into(),
            Value::from(cpu.frequency_control),
          ),
          ("governors".into(), Value::from(cpu.governors)),
          ("epp-values".into(), Value::from(cpu.epps)),
          ("epb-values".into(), Value::from(cpu.epbs)),
        ])
      })
      .collect()
  }

  async fn get_boost_capabilities(&self) -> HashMap<String, Value<'_>> {
    let Some(boost) = self.state.read().await.boost_capabilities() else {
      return HashMap::new();
    };
    let paths: Vec<_> = boost
      .paths
      .iter()
      .map(|path| path.display().to_string())
      .collect();
    HashMap::from([
      (
        "scope".into(),
        Value::from(if boost.global { "global" } else { "policy" }),
      ),
      ("control-paths".into(), Value::from(paths)),
      ("inverted".into(), Value::from(boost.inverted)),
    ])
  }

  async fn get_recent_errors(&self) -> Vec<String> {
    self.state.read().await.recent_errors()
  }

  async fn get_applied_settings(
    &self,
  ) -> Vec<(String, String, String, String)> {
    let state = self.state.read().await;
    state
      .applied_settings()
      .into_iter()
      .map(|setting| {
        let verification = setting.verification().to_owned();
        (
          setting.path.display().to_string(),
          setting.requested,
          setting.observed.unwrap_or_default(),
          verification,
        )
      })
      .collect()
  }

  async fn get_config(&self) -> String {
    let state = self.state.read().await;
    state.config().to_owned()
  }

  #[zbus(signal)]
  pub async fn applied_rules_changed(
    emitter: &SignalEmitter<'_>,
  ) -> zbus::Result<()>;

  #[zbus(signal)]
  pub async fn applied_settings_changed(
    emitter: &SignalEmitter<'_>,
  ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use tokio::time::timeout;
  use zbus::{
    Proxy,
    connection,
    zvariant::OwnedValue,
  };

  use super::*;
  use crate::{
    dbus::test_support::TestBus,
    test_support::SystemFixture,
  };

  #[tokio::test]
  async fn exported_boost_capabilities_report_selected_global_control() {
    let fixture = SystemFixture::new();
    let control = "/sys/devices/system/cpu/intel_pstate/no_turbo";
    fixture.write(control, "1");
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let state = Arc::new(RwLock::new(DaemonState::new(String::new(), 0)));
      let server = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("dev.notashelf.Watt")
        .unwrap()
        .serve_at("/dev/notashelf/Watt", WattInterface::new(state))
        .unwrap()
        .build()
        .await
        .unwrap();
      let client = connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
      let proxy = Proxy::new(
        &client,
        "dev.notashelf.Watt",
        "/dev/notashelf/Watt",
        "dev.notashelf.Watt",
      )
      .await
      .unwrap();
      let result: HashMap<String, OwnedValue> =
        proxy.call("GetBoostCapabilities", &()).await.unwrap();
      assert_eq!(<&str>::try_from(&result["scope"]).unwrap(), "global");
      assert!(bool::try_from(&result["inverted"]).unwrap());
      let paths =
        Vec::<String>::try_from(result["control-paths"].try_clone().unwrap())
          .unwrap();
      assert_eq!(paths, vec![control.to_owned()]);
      assert_eq!(fixture.write_count(control), 0);
      client.close().await.unwrap();
      server.close().await.unwrap();
    })
    .await
    .unwrap();
  }
}
