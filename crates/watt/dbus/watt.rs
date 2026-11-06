use std::{
  collections::HashMap,
  future::Future,
  sync::Arc,
};

use dbus::{
  arg::{
    Append,
    AppendAll,
    Arg,
    ArgAll,
    PropMap,
    RefArg,
  },
  nonblock::SyncConnection,
  strings::Member,
};
use dbus_crossroads::{
  Crossroads,
  IfaceToken,
  MethodErr,
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

use super::{
  auth::{
    Caller,
    authorize,
  },
  ppd::{
    boxed,
    prop_map,
  },
};
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

pub(crate) const PATH: &str = "/dev/notashelf/Watt";
pub(crate) const INTERFACE: &str = "dev.notashelf.Watt";
static VERIFICATION_SLOT: Semaphore = Semaphore::const_new(1);
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 4096;
const MAX_SNAPSHOT_ROWS: usize = 4096;

type DiagnosticRow = (String, String, u16, String, String);
type PolicyRow = (String, String, Vec<u32>, String);
type SettingRow = (String, String, String, String);

fn verify_contents(
  contents: &str,
  hardware: bool,
) -> Result<Vec<Diagnostic>, MethodErr> {
  let config = DaemonConfig::parse(contents)
    .map_err(|error| invalid_args(format!("invalid configuration: {error}")))?;
  let mut diagnostics = config.verify_static();
  if hardware
    && !diagnostics
      .iter()
      .any(|diagnostic| diagnostic.severity == Severity::Error)
  {
    diagnostics.extend(config.verify_hardware().map_err(|error| {
      failed(format!("hardware verification failed: {error}"))
    })?);
  }
  if diagnostics.len() > MAX_DIAGNOSTICS {
    return Err(limits(format!(
      "verification produced more than {MAX_DIAGNOSTICS} diagnostics"
    )));
  }
  Ok(diagnostics)
}

#[derive(Clone)]
pub struct WattInterface {
  state:  Arc<RwLock<DaemonState>>,
  reload: Option<mpsc::Sender<ReloadRequest>>,
}

pub struct ReloadRequest {
  pub response: oneshot::Sender<misstep::Result<()>>,
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

  async fn reload(
    &self,
    connection: Arc<SyncConnection>,
    caller: Caller,
  ) -> Result<(), MethodErr> {
    let Some(reload) = &self.reload else {
      return Err(not_supported("reload is unavailable"));
    };
    authorize(connection, caller, "dev.notashelf.watt.reload")
      .await
      .map_err(|error| access_denied(error.to_string()))?;
    let (response, result) = oneshot::channel();
    reload
      .try_send(ReloadRequest { response })
      .map_err(|_| limits("reload is busy"))?;
    result
      .await
      .map_err(|_| failed("reload request was cancelled"))?
      .map_err(|error| failed(format!("reload failed: {error}")))
  }

  async fn verify_configuration(
    &self,
    contents: String,
    hardware: bool,
  ) -> Result<Vec<DiagnosticRow>, MethodErr> {
    if contents.len() > MAX_CONFIG_BYTES {
      return Err(limits("configuration exceeds 1 MiB"));
    }
    let permit = VERIFICATION_SLOT
      .try_acquire()
      .map_err(|_| limits("configuration verification is busy"))?;
    let diagnostics = spawn_blocking(move || {
      let _permit = permit;
      verify_contents(&contents, hardware)
    })
    .await
    .map_err(|error| {
      failed(format!("verification worker failed: {error}"))
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

  async fn get_status(&self) -> PropMap {
    let state = self.state.read().await;
    let mut status = PropMap::new();
    if let Some(log) = state.latest_cpu_log() {
      status.insert("cpu-usage".to_owned(), boxed(log.usage * 100.0));
      if let Some(temperature) = log.temperature {
        status.insert("cpu-temperature".to_owned(), boxed(temperature));
      }
    }
    status.insert(
      "profile".to_owned(),
      boxed(state.active_profile().as_str().to_owned()),
    );
    status.insert("is-discharging".to_owned(), boxed(state.is_discharging()));
    status.insert("error-count".to_owned(), boxed(state.error_count() as u64));
    status.insert(
      "performance-degraded".to_owned(),
      boxed(state.performance_degraded().unwrap_or_default().to_owned()),
    );
    if let Some(delay) = state.polling_delay() {
      status.insert(
        "polling-delay-ms".to_owned(),
        boxed(delay.as_millis() as u64),
      );
    }
    if let Some(error) = state.latest_error() {
      status.insert("latest-error".to_owned(), boxed(error.to_owned()));
    }
    status
  }

  async fn get_cpu_policies(&self) -> Result<Vec<PolicyRow>, MethodErr> {
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

  async fn get_applied_rules(&self) -> Result<Vec<String>, MethodErr> {
    let rules = self.state.read().await.last_applied_rules();
    check_snapshot_bound(rules.len())?;
    Ok(rules)
  }

  async fn get_rules(&self) -> Result<Vec<(String, u16)>, MethodErr> {
    let state = self.state.read().await;
    check_snapshot_bound(state.rule_metadata().len())?;
    Ok(state.rule_metadata().to_vec())
  }

  async fn get_cpu_capabilities(&self) -> Result<Vec<PropMap>, MethodErr> {
    let cpus = self.state.read().await.cpu_capabilities();
    check_snapshot_bound(cpus.len())?;
    Ok(
      cpus
        .into_iter()
        .map(|cpu| {
          let mut values = prop_map([
            ("cpu", boxed(cpu.number)),
            ("policy", boxed(cpu.policy.unwrap_or_default())),
            ("frequency-control", boxed(cpu.frequency_control)),
            ("governors", boxed(cpu.governors)),
            ("epp-values", boxed(cpu.epps)),
            ("epb-values", boxed(cpu.epbs)),
          ]);
          if let Some(minimum) = cpu.minimum_khz {
            values.insert("min-khz".into(), boxed(minimum));
          }
          if let Some(maximum) = cpu.maximum_khz {
            values.insert("max-khz".into(), boxed(maximum));
          }
          values
        })
        .collect(),
    )
  }

  async fn get_boost_capabilities(&self) -> PropMap {
    let state = self.state.read().await;
    let Some(boost) = state.boost_capabilities() else {
      return PropMap::new();
    };
    let paths: Vec<_> = boost
      .paths
      .iter()
      .map(|path| path.display().to_string())
      .collect();
    prop_map([
      (
        "scope",
        boxed(if boost.global { "global" } else { "policy" }.to_owned()),
      ),
      ("control-paths", boxed(paths)),
      ("inverted", boxed(boost.inverted)),
    ])
  }

  async fn get_recent_errors(&self) -> Vec<String> {
    self.state.read().await.recent_errors()
  }

  async fn get_applied_settings(&self) -> Result<Vec<SettingRow>, MethodErr> {
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
  ) -> Result<Vec<PropMap>, MethodErr> {
    let state = self.state.read().await;
    let settings = state.applied_settings();
    check_snapshot_bound(settings.len())?;
    Ok(
      settings
        .into_iter()
        .map(|setting| {
          let cpus = state.setting_cpus(&setting.path);
          let mut detail = prop_map([
            ("path", boxed(setting.path.display().to_string())),
            ("verification", boxed(setting.verification().to_owned())),
            ("requested", boxed(setting.requested)),
            ("observed", boxed(setting.observed.unwrap_or_default())),
          ]);
          if let Some(rule) = setting.rule {
            detail.insert("rule".into(), boxed(rule.name));
            detail.insert("priority".into(), boxed(rule.priority));
          }
          if !cpus.is_empty() {
            detail.insert("cpus".into(), boxed(cpus));
          }
          detail
        })
        .collect(),
    )
  }

  async fn get_device_capabilities(&self) -> Result<Vec<PropMap>, MethodErr> {
    let devices = self.state.read().await.device_capabilities().to_vec();
    check_snapshot_bound(devices.len())?;
    Ok(
      devices
        .into_iter()
        .map(|device| {
          prop_map([
            ("subsystem", boxed(device.subsystem)),
            ("name", boxed(device.name)),
            ("path", boxed(device.path.display().to_string())),
            ("controls", boxed(device.controls)),
            (
              "available-values",
              boxed(
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

  async fn get_polling_diagnostics(&self) -> PropMap {
    let diagnostics = self.state.read().await.polling_diagnostics().clone();
    let metrics: Vec<PropMap> = diagnostics
      .duration_metrics
      .into_iter()
      .map(|metric| {
        let mut values = prop_map([
          ("metric", boxed(metric.metric)),
          ("rule", boxed(metric.rule)),
          ("priority", boxed(metric.priority)),
          ("required-samples", boxed(metric.required_samples as u64)),
          ("sample-count", boxed(metric.sample_count as u64)),
          ("available", boxed(metric.available)),
        ]);
        if let Some(window) = metric.window_seconds {
          values.insert("window-seconds".into(), boxed(window));
        }
        values
      })
      .collect();
    prop_map([
      (
        "base-interval-seconds",
        boxed(diagnostics.base_interval_seconds),
      ),
      (
        "current-interval-seconds",
        boxed(diagnostics.current_interval_seconds),
      ),
      (
        "next-interval-seconds",
        boxed(diagnostics.next_interval_seconds),
      ),
      ("idle-factor", boxed(diagnostics.idle_factor)),
      ("volatility-factor", boxed(diagnostics.volatility_factor)),
      (
        "requested-delay-seconds",
        boxed(diagnostics.requested_delay_seconds),
      ),
      (
        "smoothed-delay-seconds",
        boxed(diagnostics.smoothed_delay_seconds),
      ),
      (
        "clamped-delay-seconds",
        boxed(diagnostics.clamped_delay_seconds),
      ),
      (
        "evaluation-elapsed-seconds",
        boxed(diagnostics.evaluation_elapsed_seconds),
      ),
      (
        "cpu-sample-count",
        boxed(diagnostics.cpu_sample_count as u64),
      ),
      (
        "temperature-sample-count",
        boxed(diagnostics.temperature_sample_count as u64),
      ),
      (
        "power-sample-count",
        boxed(diagnostics.power_sample_count as u64),
      ),
      (
        "cpu-usage-volatility-available",
        boxed(diagnostics.cpu_usage_volatility_available),
      ),
      (
        "cpu-temperature-volatility-available",
        boxed(diagnostics.cpu_temperature_volatility_available),
      ),
      (
        "power-discharge-rate-available",
        boxed(diagnostics.power_discharge_rate_available),
      ),
      (
        "volatility-required-samples",
        boxed(diagnostics.volatility_required_samples as u64),
      ),
      (
        "power-discharge-required-samples",
        boxed(diagnostics.power_discharge_required_samples as u64),
      ),
      ("duration-metrics", boxed(metrics)),
      (
        "duration-metrics-omitted",
        boxed(diagnostics.duration_metrics_omitted as u64),
      ),
    ])
  }

  async fn get_config(&self) -> Result<String, MethodErr> {
    let state = self.state.read().await;
    if state.config().len() > MAX_CONFIG_BYTES {
      return Err(limits("active configuration exceeds 1 MiB"));
    }
    Ok(state.config().to_owned())
  }
}

pub(crate) fn register(
  crossroads: &mut Crossroads,
  connection: Arc<SyncConnection>,
) -> IfaceToken<WattInterface> {
  crossroads.register(INTERFACE, move |builder| {
    builder.method_with_cr_async("Reload", (), (), {
      let connection = connection.clone();
      move |mut context, crossroads, (): ()| {
        let data = data(crossroads, context.path());
        let connection = connection.clone();
        let caller = Caller::from_message(context.message())
          .map_err(|error| access_denied(error.to_string()));
        async move {
          let result = match caller {
            Ok(caller) => data.reload(connection, caller).await,
            Err(error) => Err(error),
          };
          context.reply(result)
        }
      }
    });
    builder.method_with_cr_async(
      "VerifyConfiguration",
      ("contents", "hardware"),
      ("diagnostics",),
      move |mut context, crossroads, (contents, hardware): (String, bool)| {
        let data = data(crossroads, context.path());
        async move {
          context.reply(
            data
              .verify_configuration(contents, hardware)
              .await
              .map(|value| (value,)),
          )
        }
      },
    );

    property_const(builder, "Version", env!("CARGO_PKG_VERSION").to_owned());
    property_const(builder, "InterfaceVersion", 1_u32);
    property_u32(builder, "RuleCount", |state| state.rule_count() as u32);
    property_u32(builder, "CpuCount", |state| state.cpu_count() as u32);
    property_u32(builder, "ErrorCount", |state| state.error_count() as u32);
    property_string(builder, "PerformanceDegraded", |state| {
      state.performance_degraded().unwrap_or_default().to_owned()
    });
    property_string(builder, "LatestError", |state| {
      state.latest_error().unwrap_or_default().to_owned()
    });
    builder
      .property::<Vec<PropMap>, _>("DegradedConditions")
      .get_async(|mut context, data: &mut WattInterface| {
        let state = data.state.clone();
        async move {
          let conditions = state
            .read()
            .await
            .degraded_conditions()
            .iter()
            .map(|condition| {
              prop_map([
                ("context", boxed(condition.context.clone())),
                ("message", boxed(condition.message.clone())),
              ])
            })
            .collect();
          context.reply(Ok(conditions))
        }
      });

    method0(builder, "GetStatus", ("status",), |data| {
      async move { Ok((data.get_status().await,)) }
    });
    method0(builder, "GetCpuPolicies", ("policies",), |data| {
      async move { data.get_cpu_policies().await.map(|value| (value,)) }
    });
    method0(builder, "GetAppliedRules", ("rules",), |data| {
      async move { data.get_applied_rules().await.map(|value| (value,)) }
    });
    method0(builder, "GetRules", ("rules",), |data| {
      async move { data.get_rules().await.map(|value| (value,)) }
    });
    method0(builder, "GetCpuCapabilities", ("cpus",), |data| {
      async move { data.get_cpu_capabilities().await.map(|value| (value,)) }
    });
    method0(builder, "GetBoostCapabilities", ("boost",), |data| {
      async move { Ok((data.get_boost_capabilities().await,)) }
    });
    method0(builder, "GetRecentErrors", ("errors",), |data| {
      async move { Ok((data.get_recent_errors().await,)) }
    });
    method0(builder, "GetAppliedSettings", ("settings",), |data| {
      async move { data.get_applied_settings().await.map(|value| (value,)) }
    });
    method0(builder, "GetAppliedSettingDetails", ("settings",), |data| {
      async move {
        data
          .get_applied_setting_details()
          .await
          .map(|value| (value,))
      }
    });
    method0(builder, "GetDeviceCapabilities", ("devices",), |data| {
      async move { data.get_device_capabilities().await.map(|value| (value,)) }
    });
    method0(builder, "GetPollingDiagnostics", ("diagnostics",), |data| {
      async move { Ok((data.get_polling_diagnostics().await,)) }
    });
    method0(builder, "GetConfig", ("config",), |data| {
      async move { data.get_config().await.map(|value| (value,)) }
    });

    builder.signal::<(), _>("AppliedRulesChanged", ());
    builder.signal::<(), _>("AppliedSettingsChanged", ());
    builder.signal::<(bool, String), _>(
      "ConfigurationReloaded",
      ("success", "message"),
    );
    builder.signal::<(), _>("CapabilitiesChanged", ());
  })
}

fn data(
  crossroads: &mut Crossroads,
  path: &dbus::Path<'static>,
) -> WattInterface {
  crossroads.data_mut::<WattInterface>(path).unwrap().clone()
}

fn method0<OA, N, F, Fut>(
  builder: &mut dbus_crossroads::IfaceBuilder<WattInterface>,
  name: N,
  output_args: OA::strs,
  mut call: F,
) where
  OA: ArgAll + AppendAll + 'static,
  N: Into<Member<'static>>,
  F: FnMut(WattInterface) -> Fut + Send + 'static,
  Fut: Future<Output = Result<OA, MethodErr>> + Send + 'static,
{
  builder.method_with_cr_async(
    name,
    (),
    output_args,
    move |mut context, cr, (): ()| {
      let data = data(cr, context.path());
      let future = call(data);
      async move { context.reply(future.await) }
    },
  );
}

fn property_const<T>(
  builder: &mut dbus_crossroads::IfaceBuilder<WattInterface>,
  name: &str,
  value: T,
) where
  T: Arg + Append + RefArg + Clone + Send + 'static,
{
  builder
    .property::<T, _>(name)
    .get(move |_, _: &mut WattInterface| Ok(value.clone()))
    .emits_changed_const();
}

fn property_u32(
  builder: &mut dbus_crossroads::IfaceBuilder<WattInterface>,
  name: &str,
  value: fn(&DaemonState) -> u32,
) {
  builder.property::<u32, _>(name).get_async(
    move |mut context, data: &mut WattInterface| {
      let state = data.state.clone();
      async move {
        let state = state.read().await;
        let value = value(&state);
        context.reply(Ok(value))
      }
    },
  );
}

fn property_string(
  builder: &mut dbus_crossroads::IfaceBuilder<WattInterface>,
  name: &str,
  value: fn(&DaemonState) -> String,
) {
  builder.property::<String, _>(name).get_async(
    move |mut context, data: &mut WattInterface| {
      let state = data.state.clone();
      async move {
        let state = state.read().await;
        let value = value(&state);
        context.reply(Ok(value))
      }
    },
  );
}

fn check_snapshot_bound(rows: usize) -> Result<(), MethodErr> {
  if rows > MAX_SNAPSHOT_ROWS {
    return Err(limits(format!("snapshot exceeds {MAX_SNAPSHOT_ROWS} rows")));
  }
  Ok(())
}

fn failed(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.Failed", message.into()).into()
}

fn invalid_args(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.InvalidArgs", message.into()).into()
}

fn limits(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.LimitsExceeded", message.into()).into()
}

fn access_denied(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.AccessDenied", message.into()).into()
}

fn not_supported(message: impl Into<String>) -> MethodErr {
  ("org.freedesktop.DBus.Error.NotSupported", message.into()).into()
}
