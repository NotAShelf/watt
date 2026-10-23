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
