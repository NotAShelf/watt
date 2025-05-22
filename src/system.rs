use anyhow::{Context, bail};

use crate::{cpu, fs, power_supply};

pub struct System {
    pub is_ac: bool,

    pub load_average_1min: f64,
    pub load_average_5min: f64,
    pub load_average_15min: f64,

    pub cpus: Vec<cpu::Cpu>,
    pub power_supplies: Vec<power_supply::PowerSupply>,
}

impl System {
    pub fn new() -> anyhow::Result<Self> {
        let mut system = Self {
            is_ac: false,

            cpus: Vec::new(),
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

    fn is_desktop(&mut self) -> anyhow::Result<bool> {
        if let Some(chassis_type) =
            fs::read("/sys/class/dmi/id/chassis_type").context("failed to read chassis type")?
        {
            // 3=Desktop, 4=Low Profile Desktop, 5=Pizza Box, 6=Mini Tower
            // 7=Tower, 8=Portable, 9=Laptop, 10=Notebook, 11=Hand Held, 13=All In One
            // 14=Sub Notebook, 15=Space-saving, 16=Lunch Box, 17=Main Server Chassis
            match chassis_type.trim() {
                // Desktop form factors.
                "3" | "4" | "5" | "6" | "7" | "15" | "16" | "17" => {
                    return Ok(true);
                }
                // Laptop form factors.
                "9" | "10" | "14" => {
                    return Ok(false);
                }

                // Unknown, continue with other checks
                _ => {}
            }
        }

        // Check CPU power policies, desktops often don't have these
        let power_saving_exists = fs::exists("/sys/module/intel_pstate/parameters/no_hwp")
            || fs::exists("/sys/devices/system/cpu/cpufreq/conservative");

        if !power_saving_exists {
            return Ok(true); // Likely a desktop.
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

        // Default to assuming desktop if we can't determine.
        Ok(true)
    }

    fn rescan_load_average(&mut self) -> anyhow::Result<()> {
        let content = fs::read("/proc/loadavg")
            .context("failed to read load average")?
            .context("load average file doesn't exist, are you on linux?")?;

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
