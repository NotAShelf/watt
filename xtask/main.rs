use std::{
  error,
  fs,
  io,
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
    /// Directory to install shell completions.
    #[arg(long, default_value = "completions")]
    completions_dir: PathBuf,

    /// Directory to install multicall binaries.
    #[arg(long, default_value = "bin")]
    bin_dir: PathBuf,

    /// Path to the watt binary.
    #[arg(long, default_value = "target/release/watt")]
    watt_binary: PathBuf,
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
      let mut command = binary.command();
      command.set_bin_name(binary.name());
      command.build();

      shell.generator().generate(&command, &mut io::stdout());
    },

    Command::Dist {
      completions_dir,
      bin_dir,
      watt_binary,
    } => {
      if let Err(error) =
        create_dist_files(&completions_dir, &bin_dir, &watt_binary)
      {
        eprintln!("error creating distribution files: {error}");
        process::exit(1);
      }
    },
  }
}

/// Create distribution files.
fn create_dist_files(
  completions_dir: &Path,
  bin_dir: &Path,
  watt_path: &Path,
) -> Result<(), Box<dyn error::Error>> {
  println!("creating distribution files...");

  // Create directories if they don't exist.
  fs::create_dir_all(completions_dir)?;
  fs::create_dir_all(bin_dir)?;

  if !watt_path.exists() {
    return Err(
      format!(
        "watt binary not found at: {path}",
        path = watt_path.display(),
      )
      .into(),
    );
  }

  println!("generating shell completions...");

  let shells = [
    Shell::Bash,
    Shell::Elvish,
    Shell::Fish,
    Shell::PowerShell,
    Shell::Zsh,
    Shell::Nushell,
  ];

  let binaries = [Binary::Watt, Binary::Cpu, Binary::Power];

  for shell in shells {
    for binary in binaries {
      let (prefix, ext) = shell.file_parts();
      let mut path =
        completions_dir.join(format!("{prefix}{name}", name = binary.name()));
      path.set_extension(ext);

      let mut file = fs::File::create(&path)?;

      let mut command = binary.command();
      command.set_bin_name(binary.name());
      command.build();

      shell.generator().generate(&command, &mut file);

      println!("  created: {path}", path = path.display());
    }
  }

  // Create multicall binaries (hardlinks or copies)
  // Ime softlinks work too but cube said hardlinks reeeee
  // and all that jazz. - raf
  //
  // Since hard links don't occupy any extra space and prevent
  // stupid programs from canonicalizing their way into wrong
  // behaviour, they should be used. xcode-select on MacOS does
  // it, uutils-coreutils does it, busybox does it. - Cube
  println!("creating multicall binaries...");

  let multicall_binaries = [Binary::Cpu, Binary::Power];
  let bin_path = Path::new(bin_dir);

  for &binary in &multicall_binaries {
    let target_path = bin_path.join(binary.name());

    if target_path.exists() {
      fs::remove_file(&target_path)?;
    }

    match fs::hard_link(watt_path, &target_path) {
      Ok(()) => {
        println!(
          "  created hardlink: {target} points to {watt}",
          target = target_path.display(),
          watt = watt_path.display(),
        );
      },
      Err(e) => {
        eprintln!(
          "  warning: could not create hardlink for {binary}: {e}",
          binary = binary.name(),
        );
        eprintln!("  warning: falling back to copying binary...");

        // Fallback: copy the binary.
        fs::copy(watt_path, &target_path)?;

        // ...and make it executable.
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&target_path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        fs::set_permissions(&target_path, perms)?;

        println!("  created copy: {}", target_path.display());
      },
    }
  }

  println!("distribution files created successfully!");
  println!();
  println!(
    "shell completions are in: {completions_dir}",
    completions_dir = completions_dir.display(),
  );
  println!(
    "multicall binaries are in: {bin_dir}",
    bin_dir = bin_dir.display(),
  );
  println!();

  Ok(())
}
