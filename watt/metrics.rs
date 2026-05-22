use std::{
  fmt::Write as _,
  net::SocketAddr,
  sync::Arc,
  thread,
};

use anyhow::Context as _;
use tiny_http::{
  Header,
  Method,
  Response,
  Server,
  StatusCode,
};
use tokio::sync::RwLock;

use crate::{
  config,
  profile,
  system::DaemonState,
};

pub fn start(
  config: &config::MetricsConfig,
  state: Arc<RwLock<DaemonState>>,
) -> anyhow::Result<()> {
  let address = SocketAddr::new(config.listen_addr, config.port);
  let server = Server::http(address)
    .map_err(|err| anyhow::anyhow!(err))
    .with_context(|| {
      format!("failed to bind metrics HTTP server to {address}")
    })?;

  thread::Builder::new()
    .name("watt-metrics".to_owned())
    .spawn(move || serve(server, state))
    .context("failed to spawn metrics server thread")?;

  log::info!("serving Prometheus metrics at http://{address}/metrics",);

  Ok(())
}

fn serve(server: Server, state: Arc<RwLock<DaemonState>>) {
  for request in server.incoming_requests() {
    let response =
      if request.method() == &Method::Get && request.url() == "/metrics" {
        let metrics = render_metrics(&state);
        let content_type = Header::from_bytes(
          b"Content-Type",
          b"text/plain; version=0.0.4; charset=utf-8",
        )
        .expect("static metrics content type header is valid");

        Response::from_string(metrics).with_header(content_type)
      } else {
        Response::from_string("not found\n").with_status_code(StatusCode(404))
      };

    if let Err(error) = request.respond(response) {
      log::warn!("failed to respond to metrics request: {error}");
    }
  }

  log::error!("metrics HTTP server loop exited unexpectedly");
}

fn render_metrics(state: &RwLock<DaemonState>) -> String {
  let state = state.blocking_read();
  let mut metrics = String::new();

  metric_help(
    &mut metrics,
    "watt_rule_count",
    "Configured daemon rule count.",
  );
  metric_type(&mut metrics, "watt_rule_count", "gauge");
  metric(&mut metrics, "watt_rule_count", state.rule_count() as f64);

  metric_help(&mut metrics, "watt_cpu_count", "Detected CPU count.");
  metric_type(&mut metrics, "watt_cpu_count", "gauge");
  metric(&mut metrics, "watt_cpu_count", state.cpu_count() as f64);

  if let Some(cpu_log) = state.latest_cpu_log() {
    metric_help(
      &mut metrics,
      "watt_cpu_usage_ratio",
      "CPU usage ratio from 0 to 1.",
    );
    metric_type(&mut metrics, "watt_cpu_usage_ratio", "gauge");
    metric(&mut metrics, "watt_cpu_usage_ratio", cpu_log.usage);

    metric_help(
      &mut metrics,
      "watt_cpu_load_average_1m",
      "One minute CPU load average.",
    );
    metric_type(&mut metrics, "watt_cpu_load_average_1m", "gauge");
    metric(
      &mut metrics,
      "watt_cpu_load_average_1m",
      cpu_log.load_average,
    );

    if let Some(temperature) = cpu_log.temperature {
      metric_help(
        &mut metrics,
        "watt_cpu_temperature_celsius",
        "CPU temperature in degrees Celsius.",
      );
      metric_type(&mut metrics, "watt_cpu_temperature_celsius", "gauge");
      metric(&mut metrics, "watt_cpu_temperature_celsius", temperature);
    }
  }

  metric_help(
    &mut metrics,
    "watt_power_supply_discharging",
    "Whether the system is currently discharging.",
  );
  metric_type(&mut metrics, "watt_power_supply_discharging", "gauge");
  metric(
    &mut metrics,
    "watt_power_supply_discharging",
    u8::from(state.is_discharging()) as f64,
  );

  metric_help(
    &mut metrics,
    "watt_active_profile",
    "Active power profile labelled by profile name.",
  );
  metric_type(&mut metrics, "watt_active_profile", "gauge");

  let active_profile = state.active_profile();
  for profile in profile::PowerProfile::all() {
    let value = u8::from(profile == active_profile) as f64;
    labelled_metric(
      &mut metrics,
      "watt_active_profile",
      "profile",
      profile.as_str(),
      value,
    );
  }

  metrics
}

fn metric_help(metrics: &mut String, name: &str, help: &str) {
  let _ = writeln!(metrics, "# HELP {name} {help}");
}

fn metric_type(metrics: &mut String, name: &str, ty: &str) {
  let _ = writeln!(metrics, "# TYPE {name} {ty}");
}

fn metric(metrics: &mut String, name: &str, value: f64) {
  let _ = writeln!(metrics, "{name} {value}");
}

fn labelled_metric(
  metrics: &mut String,
  name: &str,
  label: &str,
  label_value: &str,
  value: f64,
) {
  let _ = writeln!(metrics, r#"{name}{{{label}="{label_value}"}} {value}"#);
}
