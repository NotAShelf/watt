use std::{
  error,
  fs,
  path::{
    Path,
    PathBuf,
  },
  process,
};

use pound::Parse as _;

/// Build Watt distribution files.
#[derive(pound::Parse)]
#[pound(name = "xtask")]
struct Cli {
  #[pound(subcommand)]
  command: Command,
}

#[derive(pound::Parse)]
enum Command {
  /// Create distribution-ready files.
  Dist {
    /// Directory to install shell completions.
    #[pound(long, default = "completions")]
    completions_dir: PathBuf,
  },
}

fn completion_shells() -> [(caress::Shell, &'static str, &'static str); 6] {
  [
    (caress::Shell::Bash, "", ""),
    (caress::Shell::Elvish, "", "elv"),
    (caress::Shell::Fish, "", "fish"),
    (caress::Shell::PowerShell, "_", "ps1"),
    (caress::Shell::Zsh, "_", ""),
    (caress::Shell::Nu, "", "nu"),
  ]
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

  for (shell, prefix, ext) in completion_shells() {
    let mut path = completions_dir.join(format!("{prefix}watt"));
    path.set_extension(ext);
    fs::write(&path, shell.script("watt"))?;

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
