mod config;
mod core;
mod cpu;
mod daemon;
mod engine;
mod monitor;
mod power_supply;

use anyhow::Context;
use clap::Parser as _;
use std::fmt::Write as _;
use std::io::Write as _;
use std::{io, process};
use yansi::Paint as _;

#[derive(clap::Parser, Debug)]
#[clap(author, version, about)]
struct Cli {
    #[command(flatten)]
    verbosity: clap_verbosity_flag::Verbosity,

    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Parser, Debug)]
enum Command {
    /// Display information.
    Info,

    /// Start the daemon.
    Start,

    /// Modify CPU attributes.
    CpuSet {
        /// The CPUs to apply the changes to. When unspecified, will be applied to all CPUs.
        #[arg(short = 'c', long = "for")]
        for_: Option<Vec<u32>>,

        /// Set the CPU governor.
        #[arg(short = 'g', long)]
        governor: Option<String>, // TODO: Validate with clap for available governors.

        /// Set CPU Energy Performance Preference (EPP). Short form: --epp.
        #[arg(short = 'p', long, alias = "epp")]
        energy_performance_preference: Option<String>,

        /// Set CPU Energy Performance Bias (EPB). Short form: --epb.
        #[arg(short = 'b', long, alias = "epb")]
        energy_performance_bias: Option<String>,

        /// Set minimum CPU frequency in MHz. Short form: --freq-min.
        #[arg(short = 'f', long, alias = "freq-min", value_parser = clap::value_parser!(u64).range(1..=10_000))]
        frequency_mhz_minimum: Option<u64>,

        /// Set maximum CPU frequency in MHz. Short form: --freq-max.
        #[arg(short = 'F', long, alias = "freq-max", value_parser = clap::value_parser!(u64).range(1..=10_000))]
        frequency_mhz_maximum: Option<u64>,

        /// Set turbo boost behaviour. Has to be for all CPUs.
        #[arg(short = 't', long, conflicts_with = "for_")]
        turbo: Option<bool>,
    },

    /// Modify power supply attributes.
    PowerSet {
        /// The power supplies to apply the changes to. When unspecified, will be applied to all power supplies.
        #[arg(short = 'p', long = "for")]
        for_: Option<Vec<String>>,

        /// Set the percentage that the power supply has to drop under for charging to start. Short form: --charge-start.
        #[arg(short = 'c', long, alias = "charge-start", value_parser = clap::value_parser!(u8).range(0..=100))]
        charge_threshold_start: Option<u8>,

        /// Set the percentage where charging will stop. Short form: --charge-end.
        #[arg(short = 'C', long, alias = "charge-end", value_parser = clap::value_parser!(u8).range(0..=100))]
        charge_threshold_end: Option<u8>,

        /// Set ACPI platform profile. Has to be for all power supplies.
        #[arg(short = 'f', long, alias = "profile", conflicts_with = "for_")]
        platform_profile: Option<String>,
    },
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbosity.log_level_filter())
        .format_timestamp(None)
        .format_module_path(false)
        .init();

    let config = config::load_config().context("failed to load config")?;

    match cli.command {
        Command::Info => todo!(),

        Command::Start => {
            daemon::run_daemon(config)?;
            Ok(())
        }

        Command::CpuSet {
            for_,
            governor,
            energy_performance_preference,
            energy_performance_bias,
            frequency_mhz_minimum,
            frequency_mhz_maximum,
            turbo,
        } => {
            let cpus = match for_ {
                Some(numbers) => {
                    let mut cpus = Vec::with_capacity(numbers.len());

                    for number in numbers {
                        cpus.push(cpu::Cpu::new(number)?);
                    }

                    cpus
                }
                None => cpu::Cpu::all()?,
            };

            for cpu in cpus {
                if let Some(governor) = governor.as_ref() {
                    cpu.set_governor(governor)?;
                }

                if let Some(epp) = energy_performance_preference.as_ref() {
                    cpu.set_epp(epp)?;
                }

                if let Some(epb) = energy_performance_bias.as_ref() {
                    cpu.set_epb(epb)?;
                }

                if let Some(mhz_minimum) = frequency_mhz_minimum {
                    cpu.set_frequency_minimum(mhz_minimum)?;
                }

                if let Some(mhz_maximum) = frequency_mhz_maximum {
                    cpu.set_frequency_maximum(mhz_maximum)?;
                }
            }

            if let Some(turbo) = turbo {
                cpu::Cpu::set_turbo(turbo)?;
            }

            Ok(())
        }

        Command::PowerSet {
            for_,
            charge_threshold_start,
            charge_threshold_end,
            platform_profile,
        } => {
            let power_supplies = match for_ {
                Some(names) => {
                    let mut power_supplies = Vec::with_capacity(names.len());

                    for name in names {
                        power_supplies.push(power_supply::PowerSupply::from_name(name)?);
                    }

                    power_supplies
                }

                None => power_supply::PowerSupply::all()?
                    .into_iter()
                    .filter(|power_supply| power_supply.threshold_config.is_some())
                    .collect(),
            };

            for power_supply in power_supplies {
                if let Some(threshold_start) = charge_threshold_start {
                    power_supply.set_charge_threshold_start(threshold_start)?;
                }

                if let Some(threshold_end) = charge_threshold_end {
                    power_supply.set_charge_threshold_end(threshold_end)?;
                }
            }

            if let Some(platform_profile) = platform_profile.as_ref() {
                power_supply::PowerSupply::set_platform_profile(platform_profile);
            }

            Ok(())
        }
    }
}

fn main() {
    let Err(error) = real_main() else {
        return;
    };

    let mut err = io::stderr();

    let mut message = String::new();
    let mut chain = error.chain().rev().peekable();

    while let Some(error) = chain.next() {
        let _ = write!(
            err,
            "{header} ",
            header = if chain.peek().is_none() {
                "error:"
            } else {
                "cause:"
            }
            .red()
            .bold(),
        );

        String::clear(&mut message);
        let _ = write!(message, "{error}");

        let mut chars = message.char_indices();

        let _ = match (chars.next(), chars.next()) {
            (Some((_, first)), Some((second_start, second))) if second.is_lowercase() => {
                writeln!(
                    err,
                    "{first_lowercase}{rest}",
                    first_lowercase = first.to_lowercase(),
                    rest = &message[second_start..],
                )
            }

            _ => {
                writeln!(err, "{message}")
            }
        };
    }

    process::exit(1);
}
