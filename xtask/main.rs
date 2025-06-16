use std::{
  fs,
  io,
  path::Path,
};

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

  /// Create distribution-ready files (completions and multicall binaries).
  Dist {
    /// Directory to install shell completions
    #[arg(long, default_value = "completions")]
    completions_dir: String,

    /// Directory to install multicall binaries
    #[arg(long, default_value = "bin")]
    bin_dir: String,

    /// Path to the watt binary
    #[arg(long, default_value = "target/release/watt")]
    watt_binary: String,
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

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum Binary {
  Watt,
  Cpu,
  Power,
}

impl Binary {
  fn name(&self) -> &'static str {
    match self {
      Self::Watt => "watt",
      Self::Cpu => "cpu",
      Self::Power => "power",
    }
  }

  fn command(&self) -> clap::Command {
    match self {
      Self::Watt => watt::WattCommand::command(),
      Self::Cpu => watt::CpuCommand::command(),
      Self::Power => watt::PowerCommand::command(),
    }
  }
}

fn main() {
  let cli = Cli::parse();

  match cli.command {
    Command::GenerateCompletions { shell, binary } => {
      generate_completion(shell, binary);
    },

    Command::Dist {
      completions_dir,
      bin_dir,
      watt_binary,
    } => {
      if let Err(e) =
        create_dist_files(&completions_dir, &bin_dir, &watt_binary)
      {
        eprintln!("Error creating distribution files: {e}");
        std::process::exit(1);
      }
    },
  }
}

fn generate_completion(shell: Shell, binary: Binary) {
  let mut command = binary.command();
  command.set_bin_name(binary.name());
  command.build();

  shell.generator().generate(&command, &mut io::stdout());
}

/// Create distribution files
fn create_dist_files(
  completions_dir: &str,
  bin_dir: &str,
  watt_binary: &str,
) -> Result<(), Box<dyn std::error::Error>> {
  println!("Creating distribution files...");

  // Create directories if they don't exist
  fs::create_dir_all(completions_dir)?;
  fs::create_dir_all(bin_dir)?;

  if !Path::new(watt_binary).exists() {
    return Err(format!("Watt binary not found at: {watt_binary}").into());
  }

  println!("Generating shell completions...");

  let shells = [
    Shell::Bash,
    Shell::Elvish,
    Shell::Fish,
    Shell::PowerShell,
    Shell::Zsh,
    Shell::Nushell,
  ];

  let binaries = [Binary::Watt, Binary::Cpu, Binary::Power];

  for &shell in &shells {
    for &binary in &binaries {
      let mut command = binary.command();
      command.set_bin_name(binary.name());
      command.build();

      let (prefix, ext) = shell.file_parts();
      let filename = match ext {
        "" => format!("{prefix}{}", binary.name()),
        ext => format!("{prefix}{}.{ext}", binary.name()),
      };

      let completion_file = Path::new(completions_dir).join(filename);
      let mut file = fs::File::create(&completion_file)?;
      shell.generator().generate(&command, &mut file);

      println!("  Created: {}", completion_file.display());
    }
  }

  // Create multicall binaries (hardlinks or copies)
  // Ime softlinks work too but cube said hardlinks reeeee
  // and all that jazz.
  println!("Creating multicall binaries...");

  let multicall_binaries = [Binary::Cpu, Binary::Power];
  let bin_path = Path::new(bin_dir);

  for &binary in &multicall_binaries {
    let target_path = bin_path.join(binary.name());

    if target_path.exists() {
      fs::remove_file(&target_path)?;
    }

    match fs::hard_link(watt_binary, &target_path) {
      Ok(()) => {
        println!(
          "  Created hardlink: {} -> {}", // XXX: is this confusing?
          target_path.display(),
          watt_binary
        );
      },
      Err(e) => {
        eprintln!(
          "  Warning: Could not create hardlink for {}: {e}",
          binary.name()
        );
        eprintln!("  Falling back to copying binary...");

        // Fallback: copy the binary
        fs::copy(watt_binary, &target_path)?;

        // ...and make it executable
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&target_path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        fs::set_permissions(&target_path, perms)?;

        println!("  Created copy: {}", target_path.display());
      },
    }
  }

  println!("Distribution files created successfully!");
  println!();
  println!("Shell completions are in: {completions_dir}/");
  println!("Multicall binaries are in: {bin_dir}/");
  println!();

  Ok(())
}
