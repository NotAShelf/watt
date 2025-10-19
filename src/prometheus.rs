// Prometheus metrics exporter for Watt
//
// This module is only compiled when the 'prometheus' feature is enabled

use crate::core::SystemReport;
use log::{debug, info};
use prometheus_exporter::prometheus::{
    Gauge, GaugeVec, IntGauge, register_gauge, register_gauge_vec, register_int_gauge,
};
use std::net::SocketAddr;
use std::sync::OnceLock;

/// Container for all Prometheus metrics
struct WattMetrics {
    // Per-core metrics (with core label)
    cpu_frequency_mhz: GaugeVec,
    cpu_usage_percent: GaugeVec,
    cpu_temperature_celsius: GaugeVec,

    // Global CPU metrics
    cpu_average_temperature_celsius: Gauge,
    cpu_turbo_enabled: IntGauge,

    // Battery metrics (with battery label)
    battery_capacity_percent: GaugeVec,
    battery_power_watts: GaugeVec,
    battery_ac_connected: IntGauge,

    // System load metrics
    system_load_1min: Gauge,
    system_load_5min: Gauge,
    system_load_15min: Gauge,

    // Info metric with labels
    info: GaugeVec,
}

/// Global metrics singleton
static METRICS: OnceLock<WattMetrics> = OnceLock::new();

/// Initialize and register all Prometheus metrics
fn init_metrics() -> Result<WattMetrics, Box<dyn std::error::Error>> {
    Ok(WattMetrics {
        // Per-core metrics
        cpu_frequency_mhz: register_gauge_vec!(
            "watt_cpu_frequency_mhz",
            "Current CPU frequency in MHz per core",
            &["core"]
        )?,
        cpu_usage_percent: register_gauge_vec!(
            "watt_cpu_usage_percent",
            "CPU usage percentage per core",
            &["core"]
        )?,
        cpu_temperature_celsius: register_gauge_vec!(
            "watt_cpu_temperature_celsius",
            "CPU temperature in Celsius per core",
            &["core"]
        )?,

        // Global CPU metrics
        cpu_average_temperature_celsius: register_gauge!(
            "watt_cpu_average_temperature_celsius",
            "Average CPU temperature across all cores"
        )?,
        cpu_turbo_enabled: register_int_gauge!(
            "watt_cpu_turbo_enabled",
            "Whether CPU turbo boost is enabled (1) or disabled (0)"
        )?,

        // Battery metrics
        battery_capacity_percent: register_gauge_vec!(
            "watt_battery_capacity_percent",
            "Battery capacity percentage",
            &["battery"]
        )?,
        battery_power_watts: register_gauge_vec!(
            "watt_battery_power_watts",
            "Battery power rate in watts (positive for charging, negative for discharging)",
            &["battery"]
        )?,
        battery_ac_connected: register_int_gauge!(
            "watt_battery_ac_connected",
            "Whether AC power is connected (1) or not (0)"
        )?,

        // System load
        system_load_1min: register_gauge!(
            "watt_system_load_1min",
            "System load average over 1 minute"
        )?,
        system_load_5min: register_gauge!(
            "watt_system_load_5min",
            "System load average over 5 minutes"
        )?,
        system_load_15min: register_gauge!(
            "watt_system_load_15min",
            "System load average over 15 minutes"
        )?,

        // Info metric with labels for system information
        info: register_gauge_vec!(
            "watt_info",
            "System information (value is always 1)",
            &["cpu_model", "architecture", "distribution", "governor"]
        )?,
    })
}

/// Get or initialize the global metrics
fn get_metrics() -> &'static WattMetrics {
    METRICS.get_or_init(|| init_metrics().expect("Failed to initialize Prometheus metrics"))
}

/// Update all Prometheus metrics from a SystemReport
pub fn update_metrics(report: &SystemReport) {
    let metrics = get_metrics();

    // Update per-core metrics
    for core in &report.cpu_cores {
        let core_label = core.core_id.to_string();

        if let Some(freq) = core.current_frequency_mhz {
            metrics
                .cpu_frequency_mhz
                .with_label_values(&[&core_label])
                .set(f64::from(freq));
        }

        if let Some(usage) = core.usage_percent {
            metrics
                .cpu_usage_percent
                .with_label_values(&[&core_label])
                .set(f64::from(usage));
        }

        if let Some(temp) = core.temperature_celsius {
            metrics
                .cpu_temperature_celsius
                .with_label_values(&[&core_label])
                .set(f64::from(temp));
        }
    }

    // Update global CPU metrics
    if let Some(avg_temp) = report.cpu_global.average_temperature_celsius {
        metrics
            .cpu_average_temperature_celsius
            .set(f64::from(avg_temp));
    }

    if let Some(turbo_enabled) = report.cpu_global.turbo_status {
        metrics.cpu_turbo_enabled.set(i64::from(turbo_enabled));
    }

    // Update battery metrics
    for battery in &report.batteries {
        let battery_label = &battery.name;

        if let Some(capacity) = battery.capacity_percent {
            metrics
                .battery_capacity_percent
                .with_label_values(&[battery_label])
                .set(f64::from(capacity));
        }

        if let Some(power) = battery.power_rate_watts {
            metrics
                .battery_power_watts
                .with_label_values(&[battery_label])
                .set(f64::from(power));
        }
    }

    // Update AC connection status (use first battery or default to true if no batteries)
    let ac_connected = if let Some(battery) = report.batteries.first() {
        battery.ac_connected
    } else {
        true // No batteries means desktop, always on AC
    };
    metrics.battery_ac_connected.set(i64::from(ac_connected));

    // Update system load metrics
    metrics
        .system_load_1min
        .set(f64::from(report.system_load.load_avg_1min));
    metrics
        .system_load_5min
        .set(f64::from(report.system_load.load_avg_5min));
    metrics
        .system_load_15min
        .set(f64::from(report.system_load.load_avg_15min));

    // Update info metric
    let governor = report
        .cpu_global
        .current_governor
        .as_deref()
        .unwrap_or("unknown");
    metrics
        .info
        .with_label_values(&[
            &report.system_info.cpu_model,
            &report.system_info.architecture,
            &report.system_info.linux_distribution,
            governor,
        ])
        .set(1.0);

    debug!("Updated Prometheus metrics");
}

/// Start the Prometheus HTTP exporter server
///
/// This function starts an HTTP server that serves metrics at /metrics endpoint.
/// The server runs in a separate thread managed by prometheus_exporter.
pub fn start_exporter(bind_address: &str, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize metrics first
    let _ = get_metrics();

    // Parse the socket address
    let addr: SocketAddr = format!("{bind_address}:{port}").parse()?;

    info!("Starting Prometheus exporter on http://{addr}/metrics");

    // Start the HTTP server
    prometheus_exporter::start(addr)?;

    info!("Prometheus exporter started successfully");

    Ok(())
}
