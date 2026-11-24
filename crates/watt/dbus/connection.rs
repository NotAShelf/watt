use std::{
  sync::Arc,
  time::Duration,
};

use dbus::{
  Error,
  nonblock::{
    Proxy,
    SyncConnection,
  },
};
use dbus_tokio::connection;
use tokio::task::JoinHandle;

pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Connection {
  inner:    Arc<SyncConnection>,
  resource: JoinHandle<()>,
}

impl Connection {
  pub(crate) fn system() -> Result<Self, Error> {
    let (resource, inner) = connection::new_system_sync()?;
    Ok(Self::spawn(resource, inner))
  }

  fn spawn(
    resource: connection::IOResource<SyncConnection>,
    inner: Arc<SyncConnection>,
  ) -> Self {
    let resource = tokio::spawn(async move {
      let error = resource.await;
      log::debug!("D-Bus connection closed: {error}");
    });
    Self { inner, resource }
  }

  pub(crate) fn inner(&self) -> &Arc<SyncConnection> {
    &self.inner
  }

  pub(crate) fn proxy<'a>(
    &'a self,
    destination: &'a str,
    path: &'a str,
  ) -> Proxy<'a, Arc<SyncConnection>> {
    Proxy::new(destination, path, CALL_TIMEOUT, self.inner.clone())
  }
}

impl Drop for Connection {
  fn drop(&mut self) {
    self.resource.abort();
  }
}
