pub struct SystemInfo {
    // Overall system details
    pub cpu_model: String,
    pub architecture: String,
    pub linux_distribution: String,
}

pub struct CpuCoreInfo {
    // Per-core data
    pub core_id: u32,
    pub current_frequency_mhz: Option<u32>,
    pub min_frequency_mhz: Option<u32>,
    pub max_frequency_mhz: Option<u32>,
    pub usage_percent: Option<f32>,
    pub temperature_celsius: Option<f32>,
}

pub struct CpuGlobalInfo {
    // System-wide CPU settings
    pub current_governor: Option<String>,
    pub available_governors: Vec<String>,
    pub turbo_status: Option<bool>, // true for enabled, false for disabled
    pub epp: Option<String>,        // Energy Performance Preference
    pub epb: Option<String>,        // Energy Performance Bias
    pub platform_profile: Option<String>,
    pub average_temperature_celsius: Option<f32>, // Average temperature across all cores
}

pub struct BatteryInfo {
    // Battery status (AC connected, charging state, capacity, power rate, charge start/stop thresholds if available).
    pub name: String,
    pub ac_connected: bool,
    pub charging_state: Option<String>, // e.g., "Charging", "Discharging", "Full"
    pub capacity_percent: Option<u8>,
    pub power_rate_watts: Option<f32>, // positive for charging, negative for discharging
    pub charge_start_threshold: Option<u8>,
    pub charge_stop_threshold: Option<u8>,
}

pub struct SystemLoad {
    // System load averages.
    pub load_avg_1min: f32,
    pub load_avg_5min: f32,
    pub load_avg_15min: f32,
}

pub struct SystemReport {
    // Now combine all the above for a snapshot of the system state.
    pub system_info: SystemInfo,
    pub cpu_cores: Vec<CpuCoreInfo>,
    pub cpu_global: CpuGlobalInfo,
    pub batteries: Vec<BatteryInfo>,
    pub system_load: SystemLoad,
    pub timestamp: std::time::SystemTime, // so we know when the report was generated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationalMode {
    Powersave,
    Performance,
}
