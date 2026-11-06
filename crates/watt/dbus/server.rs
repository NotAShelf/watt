use std::{
  fmt::Display,
  sync::{
    Arc,
    Mutex,
  },
  time::Duration,
};

use dbus::{
  Message,
  message::MatchRule,
  nonblock::{
    MsgMatch,
    SyncConnection,
    stdintf::org_freedesktop_dbus::RequestNameReply,
  },
};
use dbus_crossroads::Crossroads;
use dbus_tokio::connection::{
  self as tokio_dbus,
  IOResource,
};
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt as _;
use tokio::{
  sync::{
    RwLock,
    broadcast::{
      self,
      error::RecvError,
    },
    mpsc,
    watch,
  },
  time::{
    interval,
    sleep,
  },
};

use crate::{
  cpu::BoostCapabilities,
  dbus::{
    ppd::{
      self,
      PowerProfilesInterface,
    },
    watt::{
      self,
      ReloadRequest,
      WattInterface,
    },
  },
  fs::AppliedSetting,
  system::{
    CpuCapabilities,
    DaemonState,
    DegradedCondition,
    DeviceCapabilities,
    ReloadResult,
  },
};

type OwnerChanges = (MsgMatch, UnboundedReceiver<Message>);

pub async fn start(
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
  reload: mpsc::Sender<ReloadRequest>,
  reload_results: broadcast::Receiver<ReloadResult>,
) -> Result<(), dbus::Error> {
  log::info!("starting D-Bus server...");
  let mut attempt: u32 = 0;
  loop {
    match try_start(
      state.clone(),
      applied_rules.clone(),
      reload.clone(),
      reload_results.resubscribe(),
    )
    .await
    {
      Ok(()) => return Ok(()),
      Err(error) => {
        attempt += 1;
        log::error!("D-Bus server error on attempt {attempt}: {error}");
        if attempt >= 5 {
          log::error!("D-Bus server failed after {attempt} attempts, bailing");
          return Err(error);
        }
        let delay = Duration::from_secs(2 * attempt as u64);
        log::info!(
          "retrying D-Bus in {delay_secs}s",
          delay_secs = delay.as_secs()
        );
        sleep(delay).await;
      },
    }
  }
}

async fn try_start(
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
  reload: mpsc::Sender<ReloadRequest>,
  reload_results: broadcast::Receiver<ReloadResult>,
) -> Result<(), dbus::Error> {
  let (resource, connection) = tokio_dbus::new_system_sync()?;
  let result = run_connection(
    connection,
    state.clone(),
    applied_rules,
    reload,
    reload_results,
    resource,
  )
  .await;
  state.write().await.release_bus_holds();
  result
}

pub(crate) async fn run_connection(
  connection: Arc<SyncConnection>,
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
  reload: mpsc::Sender<ReloadRequest>,
  reload_results: broadcast::Receiver<ReloadResult>,
  resource: IOResource<SyncConnection>,
) -> Result<(), dbus::Error> {
  tokio::pin!(resource);
  let owner_changes = tokio::select! {
    result = serve(&connection, state.clone(), reload) => result?,
    error = &mut resource => return Err(connection_closed(error)),
  };
  log::info!("D-Bus server started");
  tokio::select! {
    result = monitor(
      connection,
      state,
      applied_rules,
      owner_changes,
      reload_results,
    ) => result,
    error = &mut resource => Err(connection_closed(error)),
  }
}

fn connection_closed(error: impl Display) -> dbus::Error {
  dbus::Error::new_failed(&format!("D-Bus connection closed: {error}"))
}

async fn subscribe_owners(
  connection: &Arc<SyncConnection>,
) -> Result<OwnerChanges, dbus::Error> {
  let mut rule =
    MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged");
  rule.sender = Some("org.freedesktop.DBus".into());
  rule.path = Some("/org/freedesktop/DBus".into());
  Ok(connection.add_match(rule).await?.msg_stream())
}

async fn serve(
  connection: &Arc<SyncConnection>,
  state: Arc<RwLock<DaemonState>>,
  reload: mpsc::Sender<ReloadRequest>,
) -> Result<OwnerChanges, dbus::Error> {
  // Subscribe before exposing methods that can create owner-bound holds.
  let owner_changes = subscribe_owners(connection).await?;
  let mut crossroads = Crossroads::new();
  crossroads.set_async_support(Some((
    connection.clone(),
    Box::new(|future| {
      tokio::spawn(future);
    }),
  )));
  let ppd = ppd::register(&mut crossroads, connection.clone());
  let watt = watt::register(&mut crossroads, connection.clone());
  crossroads.insert(
    ppd::PATH,
    &[ppd],
    PowerProfilesInterface::new(state.clone()),
  );
  crossroads.insert(
    watt::PATH,
    &[watt],
    WattInterface::with_reload(state, reload),
  );
  let crossroads = Arc::new(Mutex::new(crossroads));
  let dispatch = crossroads.clone();
  use dbus::channel::MatchingReceiver as _;
  connection.start_receive(
    MatchRule::new_method_call(),
    Box::new(move |message, connection| {
      if let Err(error) =
        dispatch.lock().unwrap().handle_message(message, connection)
      {
        log::warn!("failed to dispatch D-Bus message: {error:?}");
      }
      true
    }),
  );
  request_name(connection, "net.hadess.PowerProfiles").await?;
  request_name(connection, "dev.notashelf.Watt").await?;
  Ok(owner_changes)
}

async fn request_name(
  connection: &Arc<SyncConnection>,
  name: &str,
) -> Result<(), dbus::Error> {
  match connection.request_name(name, false, false, true).await? {
    RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(()),
    reply => {
      Err(dbus::Error::new_failed(&format!(
        "failed to own {name}: {reply:?}"
      )))
    },
  }
}

async fn owner_changed(
  connection: &Arc<SyncConnection>,
  state: &Arc<RwLock<DaemonState>>,
  message: Message,
) -> Result<(), dbus::Error> {
  let (name, _old_owner, new_owner): (String, String, String) =
    message.read3()?;
  if !new_owner.is_empty() || !name.starts_with(':') {
    return Ok(());
  }
  let released = state.write().await.release_profile_owner(&name);
  if released.is_empty() {
    return Ok(());
  }
  log::info!(
    "removed {} profile hold(s) for disconnected owner {name}",
    released.len()
  );
  PowerProfilesInterface::new(state.clone())
    .notify_holds_changed(connection, &released);
  Ok(())
}

struct StatusSnapshot {
  error_count:          usize,
  latest_error:         Option<String>,
  performance_degraded: Option<String>,
  settings:             Vec<AppliedSetting>,
  conditions:           Vec<DegradedCondition>,
  cpus:                 Vec<CpuCapabilities>,
  devices:              Vec<DeviceCapabilities>,
  boost:                Option<BoostCapabilities>,
}

impl StatusSnapshot {
  fn new(state: &DaemonState) -> Self {
    Self {
      error_count:          state.error_count(),
      latest_error:         state.latest_error().map(str::to_owned),
      performance_degraded: state.performance_degraded().map(str::to_owned),
      settings:             state.applied_settings(),
      conditions:           state.degraded_conditions().to_vec(),
      cpus:                 state.cpu_capabilities(),
      devices:              state.device_capabilities().to_vec(),
      boost:                state.boost_capabilities().cloned(),
    }
  }

  async fn notify_changes(
    &mut self,
    state: &Arc<RwLock<DaemonState>>,
    connection: &Arc<SyncConnection>,
  ) -> Result<(), dbus::Error> {
    let current = Self::new(&*state.read().await);
    if current.error_count != self.error_count {
      ppd::properties_changed(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "ErrorCount",
      )?;
    }
    if current.latest_error != self.latest_error {
      ppd::properties_changed(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "LatestError",
      )?;
    }
    if current.performance_degraded != self.performance_degraded {
      ppd::properties_changed(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "PerformanceDegraded",
      )?;
      ppd::properties_changed(
        connection,
        ppd::PATH,
        ppd::INTERFACE,
        "PerformanceDegraded",
      )?;
    }
    if current.conditions != self.conditions {
      ppd::properties_changed(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "DegradedConditions",
      )?;
    }
    if current.settings != self.settings {
      ppd::send_signal(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "AppliedSettingsChanged",
        (),
      )?;
    }
    if current.cpus != self.cpus
      || current.devices != self.devices
      || current.boost != self.boost
    {
      ppd::send_signal(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "CapabilitiesChanged",
        (),
      )?;
    }
    *self = current;
    Ok(())
  }
}

async fn monitor(
  connection: Arc<SyncConnection>,
  state: Arc<RwLock<DaemonState>>,
  mut applied_rules: watch::Receiver<Vec<String>>,
  (_owner_match, mut owner_changes): OwnerChanges,
  mut reload_results: broadcast::Receiver<ReloadResult>,
) -> Result<(), dbus::Error> {
  let mut ticker = interval(Duration::from_secs(1));
  let mut previous = StatusSnapshot::new(&*state.read().await);
  loop {
    tokio::select! {
      _ = ticker.tick() => previous.notify_changes(&state, &connection).await?,
      changed = applied_rules.changed() => {
        changed.map_err(|_| dbus::Error::new_failed(
          "applied rules notification channel closed"
        ))?;
        applied_rules.borrow_and_update();
        ppd::send_signal(
          &connection,
          watt::PATH,
          watt::INTERFACE,
          "AppliedRulesChanged",
          (),
        )?;
      },
      result = reload_results.recv() => {
        notify_reload_result(&connection, result)?;
      },
      message = owner_changes.next() => {
        let message = message.ok_or_else(|| dbus::Error::new_failed(
          "D-Bus owner notification stream closed"
        ))?;
        owner_changed(&connection, &state, message).await?;
      },
    }
  }
}

fn notify_reload_result(
  connection: &Arc<SyncConnection>,
  result: Result<ReloadResult, RecvError>,
) -> Result<(), dbus::Error> {
  match result {
    Ok((success, message)) => {
      ppd::send_signal(
        connection,
        watt::PATH,
        watt::INTERFACE,
        "ConfigurationReloaded",
        (success, message),
      )
    },
    Err(RecvError::Lagged(count)) => {
      log::warn!(
        "D-Bus reload notification consumer fell behind; {count} outcomes \
         were lost"
      );
      Ok(())
    },
    Err(RecvError::Closed) => {
      Err(dbus::Error::new_failed(
        "reload notification channel closed",
      ))
    },
  }
}
