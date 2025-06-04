mod cpu;
mod power_supply;
mod system;

mod fs;

mod config;
// mod core;
mod daemon;
// mod engine;
// mod monitor;

use anyhow::Context;
use clap::Parser as _;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::{io, process};
use yansi::Paint as _;

#[derive(clap::Parser, Debug)]
#[clap(author, version, about)]
struct Cli {
    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Parser, Debug)]
#[clap(multicall = true)]
enum Command {
    /// Watt daemon.
    Watt {
        #[command(flatten)]
        verbosity: clap_verbosity_flag::Verbosity,

        /// The daemon config path.
        #[arg(long, env = "WATT_CONFIG")]
        config: PathBuf,
    },

    /// CPU metadata and modification utility.
    Cpu {
        #[command(flatten)]
        verbosity: clap_verbosity_flag::Verbosity,

        #[clap(subcommand)]
        command: CpuCommand,
    },

    /// Power supply metadata and modification utility.
    Power {
        #[command(flatten)]
        verbosity: clap_verbosity_flag::Verbosity,

        #[clap(subcommand)]
        command: PowerCommand,
    },
}

#[derive(clap::Parser, Debug)]
enum CpuCommand {
    /// Modify CPU attributes.
    Set(config::CpuDelta),
}

#[derive(clap::Parser, Debug)]
enum PowerCommand {
    /// Modify power supply attributes.
    Set(config::PowerDelta),
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    yansi::whenever(yansi::Condition::TTY_AND_COLOR);

    let (Command::Watt { verbosity, .. }
    | Command::Cpu { verbosity, .. }
    | Command::Power { verbosity, .. }) = cli.command;

    env_logger::Builder::new()
        .filter_level(verbosity.log_level_filter())
        .format_timestamp(None)
        .format_module_path(false)
        .init();

    match cli.command {
        Command::Watt { config, .. } => {
            let config = config::DaemonConfig::load_from(&config)
                .context("failed to load daemon config file")?;

            daemon::run(config)
        }

        Command::Cpu {
            command: CpuCommand::Set(delta),
            ..
        } => delta.apply(),

        Command::Power {
            command: PowerCommand::Set(delta),
            ..
        } => delta.apply(),
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
