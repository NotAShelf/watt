use std::{
  collections::HashMap,
  sync::Arc,
};

use tokio::sync::RwLock;
use zbus::{
  interface,
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
    state.config.rules.len() as u32
  }

  #[zbus(property)]
  async fn cpu_count(&self) -> u32 {
    let state = self.state.read().await;
    state.system.cpus.len() as u32
  }

  async fn get_status(&self) -> HashMap<String, Value<'_>> {
    let state = self.state.read().await;
    let mut status = HashMap::new();

    if let Some(log) = state.system.cpu_log.back() {
      status.insert("cpu-usage".to_owned(), Value::from(log.usage * 100.0));
      status.insert("cpu-temperature".to_owned(), Value::from(log.temperature));
    }

    status.insert(
      "profile".to_owned(),
      Value::from(String::from(state.profile.get_effective_profile().as_str())),
    );

    status.insert(
      "is-discharging".to_owned(),
      Value::from(state.system.is_discharging()),
    );

    status
  }

  async fn get_applied_rules(&self) -> Vec<String> {
    let state = self.state.read().await;
    state.last_applied_rules.clone()
  }
}
