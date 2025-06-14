use std::io;

use clap::{
  CommandFactory,
  Parser as _,
};

#[derive(clap::Parser)]
struct Cli {
  #[clap(subcommand)]
  command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
  /// Generate completions for the specified shell.
  GenerateCompletions {
    #[arg(long)]
    shell: Shell,

    #[arg(long)]
    binary: Binary,
  },
}

#[expect(clippy::enum_variant_names)]
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
  Bash,
  Elvish,
  Fish,
  PowerShell,
  Zsh,
  Nushell,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum Binary {
  Watt,
  Cpu,
  Power,
}

fn main() {
  let cli = Cli::parse();

  match cli.command {
    Command::GenerateCompletions { shell, binary } => {
      let mut command = match binary {
        Binary::Watt => watt::WattCommand::command(),
        Binary::Cpu => watt::CpuCommand::command(),
        Binary::Power => watt::PowerCommand::command(),
      };
      command.set_bin_name(format!("{binary:?}").to_lowercase());
      command.build();

      let shell: &dyn clap_complete::Generator = match shell {
        Shell::Bash => &clap_complete::Shell::Bash,
        Shell::Elvish => &clap_complete::Shell::Elvish,
        Shell::Fish => &clap_complete::Shell::Fish,
        Shell::PowerShell => &clap_complete::Shell::PowerShell,
        Shell::Zsh => &clap_complete::Shell::Zsh,
        Shell::Nushell => &clap_complete_nushell::Nushell,
      };

      shell.generate(&command, &mut io::stdout());
    },
  }
}
