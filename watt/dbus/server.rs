use std::{
  future,
  sync::Arc,
  time::Duration,
};

use tokio::sync::RwLock;
use zbus::connection;

use crate::system::DaemonState;

pub async fn start_dbus_server(
  state: Arc<RwLock<DaemonState>>,
) -> zbus::Result<()> {
  log::info!("starting D-Bus server...");

  let mut attempt: u32 = 0;
  loop {
    match try_start(state.clone()).await {
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

async fn try_start(state: Arc<RwLock<DaemonState>>) -> zbus::Result<()> {
  let ppd = crate::dbus::ppd::PowerProfilesInterface::new(state.clone());
  let watt = crate::dbus::watt::WattInterface::new(state);

  let _connection = connection::Builder::system()?
    .name("net.hadess.PowerProfiles")?
    .name("dev.notashelf.Watt")?
    .serve_at("/net/hadess/PowerProfiles", ppd)?
    .serve_at("/dev/notashelf/Watt", watt)?
    .build()
    .await?;

  log::info!("D-Bus server started");

  // Block forever to keep the D-Bus server alive
  loop {
    future::pending::<()>().await;
  }
}
