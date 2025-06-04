use std::{collections::HashMap, path::Path};

use anyhow::{Context, bail};

use crate::{cpu, fs, power_supply};

pub struct System {
    pub is_ac: bool,

    pub load_average_1min: f64,
    pub load_average_5min: f64,
    pub load_average_15min: f64,

    pub cpus: Vec<cpu::Cpu>,
    pub cpu_temperatures: HashMap<u32, f64>,

    pub power_supplies: Vec<power_supply::PowerSupply>,
}

impl System {
    pub fn new() -> anyhow::Result<Self> {
        let mut system = Self {
            is_ac: false,

            cpus: Vec::new(),
            cpu_temperatures: HashMap::new(),

            power_supplies: Vec::new(),

            load_average_1min: 0.0,
            load_average_5min: 0.0,
            load_average_15min: 0.0,
        };

        system.rescan()?;

        Ok(system)
    }

    pub fn rescan(&mut self) -> anyhow::Result<()> {
        self.cpus = cpu::Cpu::all().context("failed to scan CPUs")?;

        self.power_supplies =
            power_supply::PowerSupply::all().context("failed to scan power supplies")?;

        self.is_ac = self
            .power_supplies
            .iter()
            .any(|power_supply| power_supply.is_ac())
            || self.is_desktop()?;

        self.rescan_load_average()?;

        Ok(())
    }

    fn rescan_temperatures(&mut self) -> anyhow::Result<()> {
        const PATH: &str = "/sys/class/hwmon";

        let mut temperatures = HashMap::new();

        for entry in fs::read_dir(PATH)
            .with_context(|| format!("failed to read hardware information from '{PATH}'"))?
            .with_context(|| format!("'{PATH}' doesn't exist, are you on linux?"))?
        {
            let entry = entry.with_context(|| format!("failed to read entry of '{PATH}'"))?;

            let entry_path = entry.path();

            let Some(name) = fs::read(entry_path.join("name")).with_context(|| {
                format!(
                    "failed to read name of hardware entry at '{path}'",
                    path = entry_path.display(),
                )
            })?
            else {
                continue;
            };

            match &*name {
                // TODO: 'zenergy' can also report those stats, I think?
                "coretemp" | "k10temp" | "zenpower" | "amdgpu" => {
                    Self::get_temperatures(&entry_path, &mut temperatures)?;
                }

                // Other CPU temperature drivers.
                _ if name.contains("cpu") || name.contains("temp") => {
                    Self::get_temperatures(&entry_path, &mut temperatures)?;
                }

                _ => {}
            }
        }

        self.cpu_temperatures = temperatures;

        Ok(())
    }

    fn get_temperatures(
        device_path: &Path,
        temperatures: &mut HashMap<u32, f64>,
    ) -> anyhow::Result<()> {
        // Increased range to handle systems with many sensors.
        for i in 1..=96 {
            let label_path = device_path.join(format!("temp{i}_label"));
            let input_path = device_path.join(format!("temp{i}_input"));

            if !label_path.exists() || !input_path.exists() {
                continue;
            }

            let Some(label) = fs::read(&label_path).with_context(|| {
                format!(
                    "failed to read hardware hardware device label from '{path}'",
                    path = label_path.display(),
                )
            })?
            else {
                continue;
            };

            // Match various common label formats:
            // "Core X", "core X", "Core-X", "CPU Core X", etc.
            let number = label
                .trim_start_matches("cpu")
                .trim_start_matches("CPU")
                .trim_start()
                .trim_start_matches("core")
                .trim_start_matches("Core")
                .trim_start()
                .trim_start_matches("tdie")
                .trim_start_matches("Tdie")
                .trim_start()
                .trim_start_matches("tctl")
                .trim_start_matches("Tctl")
                .trim_start()
                .trim_start_matches("-")
                .trim();

            let Ok(number) = number.parse::<u32>() else {
                continue;
            };

            let Some(temperature_mc) = fs::read_n::<i64>(&input_path).with_context(|| {
                format!(
                    "failed to read CPU temperature from '{path}'",
                    path = input_path.display(),
                )
            })?
            else {
                continue;
            };

            temperatures.insert(number, temperature_mc as f64 / 1000.0);
        }

        Ok(())
    }

    fn is_desktop(&mut self) -> anyhow::Result<bool> {
        if let Some(chassis_type) =
            fs::read("/sys/class/dmi/id/chassis_type").context("failed to read chassis type")?
        {
            // 3=Desktop, 4=Low Profile Desktop, 5=Pizza Box, 6=Mini Tower,
            // 7=Tower, 8=Portable, 9=Laptop, 10=Notebook, 11=Hand Held, 13=All In One,
            // 14=Sub Notebook, 15=Space-saving, 16=Lunch Box, 17=Main Server Chassis,
            // 31=Convertible Laptop
            match chassis_type.trim() {
                // Desktop form factors.
                "3" | "4" | "5" | "6" | "7" | "15" | "16" | "17" => {
                    return Ok(true);
                }

                // Laptop form factors.
                "9" | "10" | "14" | "31" => {
                    return Ok(false);
                }

                // Unknown, continue with other checks
                _ => {}
            }
        }

        // Check battery-specific ACPI paths that laptops typically have
        let laptop_acpi_paths = [
            "/sys/class/power_supply/BAT0",
            "/sys/class/power_supply/BAT1",
            "/proc/acpi/battery",
        ];

        for path in laptop_acpi_paths {
            if fs::exists(path) {
                return Ok(false); // Likely a laptop.
            }
        }

        // Check CPU power policies, desktops often don't have these
        let power_saving_exists = fs::exists("/sys/module/intel_pstate/parameters/no_hwp")
            || fs::exists("/sys/devices/system/cpu/cpufreq/conservative");

        if !power_saving_exists {
            return Ok(true); // Likely a desktop.
        }

        // Default to assuming desktop if we can't determine.
        Ok(true)
    }

    fn rescan_load_average(&mut self) -> anyhow::Result<()> {
        let content = fs::read("/proc/loadavg")
            .context("failed to read load average from '/proc/loadavg'")?
            .context("'/proc/loadavg' doesn't exist, are you on linux?")?;

        let mut parts = content.split_whitespace();

        let (Some(load_average_1min), Some(load_average_5min), Some(load_average_15min)) =
            (parts.next(), parts.next(), parts.next())
        else {
            bail!(
                "failed to parse first 3 load average entries due to there not being enough, content: {content}"
            );
        };

        self.load_average_1min = load_average_1min
            .parse()
            .context("failed to parse load average")?;
        self.load_average_5min = load_average_5min
            .parse()
            .context("failed to parse load average")?;
        self.load_average_15min = load_average_15min
            .parse()
            .context("failed to parse load average")?;

        Ok(())
    }
}
