mod config;
// mod core;
mod cpu;
// mod daemon;
// mod engine;
// mod monitor;
mod power_supply;

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
    Start {
        /// The daemon config path.
        #[arg(long, env = "SUPERFREQ_CONFIG")]
        config: PathBuf,
    },

    /// Modify CPU attributes.
    CpuSet(config::CpuDelta),

    /// Modify power supply attributes.
    PowerSet(config::PowerDelta),
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(cli.verbosity.log_level_filter())
        .format_timestamp(None)
        .format_module_path(false)
        .init();

    match cli.command {
        Command::Info => todo!(),

        Command::Start { config } => {
            let _config = config::DaemonConfig::load_from(&config)
                .context("failed to load daemon config file")?;

            // daemon::run(config)
            Ok(())
        }

        Command::CpuSet(delta) => delta.apply(),
        Command::PowerSet(delta) => delta.apply(),
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
