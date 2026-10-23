use std::{
  collections::HashMap,
  sync::Arc,
};

use tokio::{
  sync::{
    RwLock,
    Semaphore,
    mpsc,
    oneshot,
  },
  task::spawn_blocking,
};
use zbus::{
  fdo,
  interface,
  message::Header,
  object_server::SignalEmitter,
  zvariant::Value,
};

use super::auth::authorize;
use crate::{
  config::{
    DaemonConfig,
    verify::{
      Diagnostic,
      Severity,
    },
  },
  system::DaemonState,
};

static VERIFICATION_SLOT: Semaphore = Semaphore::const_new(1);
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 4096;
const MAX_SNAPSHOT_ROWS: usize = 4096;

fn verify_contents(
  contents: &str,
  hardware: bool,
) -> fdo::Result<Vec<Diagnostic>> {
  let config: DaemonConfig = toml::from_str(contents).map_err(|error| {
    fdo::Error::InvalidArgs(format!("invalid configuration: {error}"))
  })?;
  let mut diagnostics = config.verify_static();
  if hardware
    && !diagnostics
      .iter()
      .any(|diagnostic| diagnostic.severity == Severity::Error)
  {
    diagnostics.extend(config.verify_hardware().map_err(|error| {
      fdo::Error::Failed(format!("hardware verification failed: {error:#}"))
    })?);
  }
  if diagnostics.len() > MAX_DIAGNOSTICS {
    return Err(fdo::Error::LimitsExceeded(format!(
      "verification produced more than {MAX_DIAGNOSTICS} diagnostics"
    )));
  }
  Ok(diagnostics)
}

pub struct WattInterface {
  state:  Arc<RwLock<DaemonState>>,
  reload: Option<mpsc::Sender<ReloadRequest>>,
}

pub struct ReloadRequest {
  pub response: oneshot::Sender<anyhow::Result<()>>,
}

impl WattInterface {
  pub fn new(state: Arc<RwLock<DaemonState>>) -> Self {
    Self {
      state,
      reload: None,
    }
  }

  pub fn with_reload(
    state: Arc<RwLock<DaemonState>>,
    reload: mpsc::Sender<ReloadRequest>,
  ) -> Self {
    Self {
      state,
      reload: Some(reload),
    }
  }
}

fn check_snapshot_bound(rows: usize) -> fdo::Result<()> {
  if rows > MAX_SNAPSHOT_ROWS {
    return Err(fdo::Error::LimitsExceeded(format!(
      "snapshot exceeds {MAX_SNAPSHOT_ROWS} rows"
    )));
  }
  Ok(())
}

#[interface(name = "dev.notashelf.Watt")]
impl WattInterface {
  async fn reload(
    &self,
    #[zbus(connection)] connection: &zbus::Connection,
    #[zbus(header)] header: Header<'_>,
  ) -> fdo::Result<()> {
    let Some(reload) = &self.reload else {
      return Err(fdo::Error::NotSupported("reload is unavailable".into()));
    };
    authorize(connection, &header, "dev.notashelf.watt.reload")
      .await
      .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let (response, result) = oneshot::channel();
    reload
      .try_send(ReloadRequest { response })
      .map_err(|_| fdo::Error::LimitsExceeded("reload is busy".into()))?;
    result
      .await
      .map_err(|_| fdo::Error::Failed("reload request was cancelled".into()))?
      .map_err(|error| fdo::Error::Failed(format!("reload failed: {error:#}")))
  }

  async fn verify_configuration(
    &self,
    contents: String,
    hardware: bool,
  ) -> fdo::Result<Vec<(String, String, u16, String, String)>> {
    if contents.len() > MAX_CONFIG_BYTES {
      return Err(fdo::Error::LimitsExceeded(
        "configuration exceeds 1 MiB".into(),
      ));
    }
    let permit = VERIFICATION_SLOT.try_acquire().map_err(|_| {
      fdo::Error::LimitsExceeded("configuration verification is busy".into())
    })?;
    let diagnostics = spawn_blocking(move || {
      // Keep the slot until the work ends, even if the caller disconnects.
      let _permit = permit;
      verify_contents(&contents, hardware)
    })
    .await
    .map_err(|error| {
      fdo::Error::Failed(format!("verification worker failed: {error}"))
    })??;
    Ok(
      diagnostics
        .into_iter()
        .map(|diagnostic| {
          let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
          };
          (
            severity.into(),
            diagnostic.rule,
            diagnostic.priority,
            diagnostic.field,
            diagnostic.message,
          )
        })
        .collect(),
    )
  }

  #[zbus(property)]
  async fn version(&self) -> String {
    env!("CARGO_PKG_VERSION").to_owned()
  }

  #[zbus(property)]
  fn interface_version(&self) -> u32 {
    1
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

    if let Some(delay) = state.polling_delay() {
      status.insert(
        "polling-delay-ms".to_owned(),
        Value::from(delay.as_millis() as u64),
      );
    }

    if let Some(error) = state.latest_error() {
      status.insert("latest-error".to_owned(), Value::from(error.to_owned()));
    }

    status
  }

  async fn get_cpu_policies(
    &self,
  ) -> fdo::Result<Vec<(String, String, Vec<u32>, String)>> {
    let policies = self.state.read().await.cpu_policies();
    check_snapshot_bound(policies.len())?;
    Ok(
      policies
        .into_iter()
        .map(|policy| {
          (
            policy.name.clone(),
            policy.path.display().to_string(),
            policy.related_cpus.clone(),
            policy.driver.clone().unwrap_or_default(),
          )
        })
        .collect(),
    )
  }

  async fn get_applied_rules(&self) -> fdo::Result<Vec<String>> {
    let rules = self.state.read().await.last_applied_rules();
    check_snapshot_bound(rules.len())?;
    Ok(rules)
  }

  async fn get_rules(&self) -> fdo::Result<Vec<(String, u16)>> {
    let state = self.state.read().await;
    check_snapshot_bound(state.rule_metadata().len())?;
    Ok(state.rule_metadata().to_vec())
  }

  async fn get_cpu_capabilities(
    &self,
  ) -> fdo::Result<Vec<HashMap<String, Value<'_>>>> {
    let cpus = self.state.read().await.cpu_capabilities();
    check_snapshot_bound(cpus.len())?;
    Ok(
      cpus
        .into_iter()
        .map(|cpu| {
          let mut values = HashMap::from([
            ("cpu".into(), Value::from(cpu.number)),
            ("policy".into(), Value::from(cpu.policy.unwrap_or_default())),
            (
              "frequency-control".into(),
              Value::from(cpu.frequency_control),
            ),
            ("governors".into(), Value::from(cpu.governors)),
            ("epp-values".into(), Value::from(cpu.epps)),
            ("epb-values".into(), Value::from(cpu.epbs)),
          ]);
          if let Some(minimum) = cpu.minimum_khz {
            values.insert("min-khz".into(), Value::from(minimum));
          }
          if let Some(maximum) = cpu.maximum_khz {
            values.insert("max-khz".into(), Value::from(maximum));
          }
          values
        })
        .collect(),
    )
  }

  async fn get_boost_capabilities(&self) -> HashMap<String, Value<'_>> {
    let state = self.state.read().await;
    let Some(boost) = state.boost_capabilities() else {
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
  ) -> fdo::Result<Vec<(String, String, String, String)>> {
    let state = self.state.read().await;
    let settings = state.applied_settings();
    check_snapshot_bound(settings.len())?;
    Ok(
      settings
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
        .collect(),
    )
  }

  async fn get_applied_setting_details(
    &self,
  ) -> fdo::Result<Vec<HashMap<String, Value<'_>>>> {
    let settings = self.state.read().await.applied_settings();
    check_snapshot_bound(settings.len())?;
    Ok(
      settings
        .into_iter()
        .map(|setting| {
          let mut detail = HashMap::from([
            (
              "path".into(),
              Value::from(setting.path.display().to_string()),
            ),
            (
              "verification".into(),
              Value::from(setting.verification().to_owned()),
            ),
            ("requested".into(), Value::from(setting.requested)),
            (
              "observed".into(),
              Value::from(setting.observed.unwrap_or_default()),
            ),
          ]);
          if let Some(rule) = setting.rule {
            detail.insert("rule".into(), Value::from(rule.name));
            detail.insert("priority".into(), Value::from(rule.priority));
          }
          detail
        })
        .collect(),
    )
  }

  async fn get_device_capabilities(
    &self,
  ) -> fdo::Result<Vec<HashMap<String, Value<'_>>>> {
    let devices = self.state.read().await.device_capabilities().to_vec();
    check_snapshot_bound(devices.len())?;
    Ok(
      devices
        .into_iter()
        .map(|device| {
          HashMap::from([
            ("subsystem".into(), Value::from(device.subsystem)),
            ("name".into(), Value::from(device.name)),
            (
              "path".into(),
              Value::from(device.path.display().to_string()),
            ),
            ("controls".into(), Value::from(device.controls)),
            (
              "available-values".into(),
              Value::from(
                device
                  .available_values
                  .into_iter()
                  .collect::<HashMap<_, _>>(),
              ),
            ),
          ])
        })
        .collect(),
    )
  }

  async fn get_polling_diagnostics(&self) -> HashMap<String, Value<'_>> {
    let diagnostics = self.state.read().await.polling_diagnostics().clone();
    let metrics: Vec<HashMap<String, Value<'_>>> = diagnostics
      .duration_metrics
      .into_iter()
      .map(|metric| {
        let mut values = HashMap::from([
          ("metric".into(), Value::from(metric.metric)),
          ("rule".into(), Value::from(metric.rule)),
          ("priority".into(), Value::from(metric.priority)),
          (
            "required-samples".into(),
            Value::from(metric.required_samples as u64),
          ),
          (
            "sample-count".into(),
            Value::from(metric.sample_count as u64),
          ),
          ("available".into(), Value::from(metric.available)),
        ]);
        if let Some(window) = metric.window_seconds {
          values.insert("window-seconds".into(), Value::from(window));
        }
        values
      })
      .collect();
    HashMap::from([
      (
        "base-interval-seconds".into(),
        Value::from(diagnostics.base_interval_seconds),
      ),
      (
        "current-interval-seconds".into(),
        Value::from(diagnostics.current_interval_seconds),
      ),
      (
        "next-interval-seconds".into(),
        Value::from(diagnostics.next_interval_seconds),
      ),
      ("idle-factor".into(), Value::from(diagnostics.idle_factor)),
      (
        "volatility-factor".into(),
        Value::from(diagnostics.volatility_factor),
      ),
      (
        "requested-delay-seconds".into(),
        Value::from(diagnostics.requested_delay_seconds),
      ),
      (
        "smoothed-delay-seconds".into(),
        Value::from(diagnostics.smoothed_delay_seconds),
      ),
      (
        "clamped-delay-seconds".into(),
        Value::from(diagnostics.clamped_delay_seconds),
      ),
      (
        "evaluation-elapsed-seconds".into(),
        Value::from(diagnostics.evaluation_elapsed_seconds),
      ),
      (
        "cpu-sample-count".into(),
        Value::from(diagnostics.cpu_sample_count as u64),
      ),
      (
        "temperature-sample-count".into(),
        Value::from(diagnostics.temperature_sample_count as u64),
      ),
      (
        "power-sample-count".into(),
        Value::from(diagnostics.power_sample_count as u64),
      ),
      (
        "cpu-usage-volatility-available".into(),
        Value::from(diagnostics.cpu_usage_volatility_available),
      ),
      (
        "cpu-temperature-volatility-available".into(),
        Value::from(diagnostics.cpu_temperature_volatility_available),
      ),
      (
        "power-discharge-rate-available".into(),
        Value::from(diagnostics.power_discharge_rate_available),
      ),
      (
        "volatility-required-samples".into(),
        Value::from(diagnostics.volatility_required_samples as u64),
      ),
      (
        "power-discharge-required-samples".into(),
        Value::from(diagnostics.power_discharge_required_samples as u64),
      ),
      ("duration-metrics".into(), Value::from(metrics)),
      (
        "duration-metrics-omitted".into(),
        Value::from(diagnostics.duration_metrics_omitted as u64),
      ),
    ])
  }

  async fn get_config(&self) -> fdo::Result<String> {
    let state = self.state.read().await;
    if state.config().len() > MAX_CONFIG_BYTES {
      return Err(fdo::Error::LimitsExceeded(
        "active configuration exceeds 1 MiB".into(),
      ));
    }
    Ok(state.config().to_owned())
  }

  #[zbus(signal)]
  pub async fn applied_rules_changed(
    emitter: &SignalEmitter<'_>,
  ) -> zbus::Result<()>;

  #[zbus(signal)]
  pub async fn applied_settings_changed(
    emitter: &SignalEmitter<'_>,
  ) -> zbus::Result<()>;

  #[zbus(signal)]
  pub async fn configuration_reloaded(
    emitter: &SignalEmitter<'_>,
    success: bool,
    message: &str,
  ) -> zbus::Result<()>;

  #[zbus(signal)]
  pub async fn capabilities_changed(
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
  };

  use super::*;
  use crate::dbus::test_support::TestBus;

  #[tokio::test]
  async fn configuration_verification_returns_diagnostics_without_reloading() {
    let bus = TestBus::new();
    timeout(Duration::from_secs(10), async {
      let state =
        Arc::new(RwLock::new(DaemonState::new("active config".into(), 7)));
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
      let invalid =
        "[[rule]]\nname='invalid'\npriority=9\ncpu.frequency-mhz-minimum=-1\n";
      for hardware in [false, true] {
        let diagnostics: Vec<(String, String, u16, String, String)> = proxy
          .call("VerifyConfiguration", &(invalid, hardware))
          .await
          .unwrap();
        assert_eq!(diagnostics.len(), 1);
        let (severity, rule, priority, field, message) = &diagnostics[0];
        assert_eq!(severity, "error");
        assert_eq!(rule, "invalid");
        assert_eq!(*priority, 9);
        assert_eq!(field, "cpu.frequency-mhz-minimum");
        assert!(!message.is_empty());
      }
      let malformed = proxy
        .call::<_, _, Vec<(String, String, u16, String, String)>>(
          "VerifyConfiguration",
          &("[", false),
        )
        .await
        .unwrap_err();
      assert!(malformed.to_string().contains("InvalidArgs"));
      let oversized = proxy
        .call::<_, _, Vec<(String, String, u16, String, String)>>(
          "VerifyConfiguration",
          &(" ".repeat(MAX_CONFIG_BYTES + 1), false),
        )
        .await
        .unwrap_err();
      assert!(oversized.to_string().contains("LimitsExceeded"));
      let duplicates = "[[rule]]\nname='duplicate'\npriority=1\n"
        .repeat(MAX_DIAGNOSTICS / 2 + 2);
      let excessive = proxy
        .call::<_, _, Vec<(String, String, u16, String, String)>>(
          "VerifyConfiguration",
          &(duplicates, false),
        )
        .await
        .unwrap_err();
      assert!(excessive.to_string().contains("LimitsExceeded"));
      let permit = VERIFICATION_SLOT.acquire().await.unwrap();
      let busy = proxy
        .call::<_, _, Vec<(String, String, u16, String, String)>>(
          "VerifyConfiguration",
          &(invalid, false),
        )
        .await
        .unwrap_err();
      assert!(busy.to_string().contains("LimitsExceeded"));
      let active: String = proxy.call("GetConfig", &()).await.unwrap();
      assert_eq!(active, "active config");
      assert_eq!(proxy.get_property::<u32>("RuleCount").await.unwrap(), 7);
      drop(permit);
      let valid: Vec<(String, String, u16, String, String)> = proxy
        .call(
          "VerifyConfiguration",
          &("[[rule]]\nname='valid'\npriority=1\n", false),
        )
        .await
        .unwrap();
      assert!(valid.is_empty());
      client.close().await.unwrap();
      server.close().await.unwrap();
    })
    .await
    .unwrap();
  }
}
