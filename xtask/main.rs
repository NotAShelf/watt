use std::{
  error,
  fs,
  path::{
    Path,
    PathBuf,
  },
  process,
};

use clap::{
  CommandFactory,
  Parser as _,
};

#[derive(clap::Parser)]
#[command(version, about)]
struct Cli {
  #[clap(subcommand)]
  command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
  /// Create distribution-ready files (completions and multicall binaries).
  Dist {
    /// Directory to install shell completions.
    #[arg(long, default_value = "completions")]
    completions_dir: PathBuf,
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

impl Shell {
  fn generator(&self) -> &'static dyn clap_complete::Generator {
    match self {
      Self::Bash => &clap_complete::Shell::Bash,
      Self::Elvish => &clap_complete::Shell::Elvish,
      Self::Fish => &clap_complete::Shell::Fish,
      Self::PowerShell => &clap_complete::Shell::PowerShell,
      Self::Zsh => &clap_complete::Shell::Zsh,
      Self::Nushell => &clap_complete_nushell::Nushell,
    }
  }

  fn file_parts(&self) -> (&'static str, &'static str) {
    match self {
      Self::Bash => ("", ""),
      Self::Elvish => ("", "elv"),
      Self::Fish => ("", "fish"),
      Self::PowerShell => ("_", "ps1"),
      Self::Zsh => ("_", ""),
      Self::Nushell => ("", "nu"),
    }
  }
}

fn main() {
  let cli = Cli::parse();

  match cli.command {
    Command::Dist { completions_dir } => {
      if let Err(error) = create_dist_files(&completions_dir) {
        eprintln!("error creating distribution files: {error}");
        process::exit(1);
      }
    },
  }
}

/// Create distribution files.
fn create_dist_files(
  completions_dir: &Path,
) -> Result<(), Box<dyn error::Error>> {
  println!("creating distribution files...");

  // Create directories if they don't exist.
  fs::create_dir_all(completions_dir)?;

  println!("generating shell completions...");

  let shells = [
    Shell::Bash,
    Shell::Elvish,
    Shell::Fish,
    Shell::PowerShell,
    Shell::Zsh,
    Shell::Nushell,
  ];

  for shell in shells {
    let (prefix, ext) = shell.file_parts();
    let mut path = completions_dir.join(format!("{prefix}watt"));
    path.set_extension(ext);

    let mut command = watt::Cli::command();
    command.set_bin_name("watt");
    command.build();

    shell
      .generator()
      .generate(&command, &mut fs::File::create(&path)?);

    println!("  created: {path}", path = path.display());
  }

  println!("distribution files created successfully!");
  println!();
  println!(
    "shell completions are in: {completions_dir}",
    completions_dir = completions_dir.display(),
  );
  println!();

  Ok(())
}
