use std::{
  sync::Arc,
  time::Duration,
};

use tokio::sync::{
  RwLock,
  watch,
};
use zbus::{
  connection,
  object_server::SignalEmitter,
};

use crate::system::DaemonState;

pub async fn start(
  state: Arc<RwLock<DaemonState>>,
  applied_rules: watch::Receiver<Vec<String>>,
) -> zbus::Result<()> {
  log::info!("starting D-Bus server...");

  let mut attempt: u32 = 0;
  loop {
    match try_start(state.clone(), applied_rules.clone()).await {
      Ok(()) => return Ok(()),
      Err(e) => {
        attempt += 1;
        log::error!("D-Bus server error on attempt {attempt}: {e}");

        if attempt >= 5 {
          log::error!("D-Bus server failed after {attempt} attempts, bailing");
          return Err(e);
        }

        let delay = Duration::from_secs(2 * attempt as u64);
        log::info!(
          "retrying D-Bus in {delay_secs}s",
          delay_secs = delay.as_secs()
        );
        tokio::time::sleep(delay).await;
      },
    }
  }
}

async fn try_start(
  state: Arc<RwLock<DaemonState>>,
  mut applied_rules: watch::Receiver<Vec<String>>,
) -> zbus::Result<()> {
  let ppd = crate::dbus::ppd::PowerProfilesInterface::new(state.clone());
  let watt = crate::dbus::watt::WattInterface::new(state);

  let connection = connection::Builder::system()?
    .name("net.hadess.PowerProfiles")?
    .name("dev.notashelf.Watt")?
    .serve_at("/net/hadess/PowerProfiles", ppd)?
    .serve_at("/dev/notashelf/Watt", watt)?
    .build()
    .await?;

  log::info!("D-Bus server started");

  let emitter = SignalEmitter::new(&connection, "/dev/notashelf/Watt")?;

  loop {
    applied_rules.changed().await.map_err(|_| {
      zbus::Error::Failure("applied rules notification channel closed".into())
    })?;
    applied_rules.borrow_and_update();
    crate::dbus::watt::WattInterface::applied_rules_changed(&emitter).await?;
  }
}
