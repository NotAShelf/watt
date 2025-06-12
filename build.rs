use std::{
  env,
  fs,
  path::PathBuf,
};

const MULTICALL_NAMES: &[&str] = &["cpu", "power"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=target");

  let out_dir = PathBuf::from(env::var("OUT_DIR")?);
  let target = out_dir
        .parent() // target/debug/build/<pkg>-<hash>/out
        .and_then(|p| p.parent()) // target/debug/build/<pkg>-<hash>
        .and_then(|p| p.parent()) // target/debug/
        .ok_or("failed to find target directory")?;

  let main_binary_name = env::var("CARGO_PKG_NAME")?;

  let main_binary_path = target.join(&main_binary_name);

  let mut errored = false;

  for name in MULTICALL_NAMES {
    let hardlink_path = target.join(name);

    if hardlink_path.exists() {
      if hardlink_path.is_dir() {
        fs::remove_dir_all(&hardlink_path)?;
      } else {
        fs::remove_file(&hardlink_path)?;
      }
    }

    if let Err(error) = fs::hard_link(&main_binary_path, &hardlink_path) {
      println!(
        "cargo:warning=failed to create hard link '{path}': {error}",
        path = hardlink_path.display(),
      );
      errored = true;
    }
  }

  if errored {
    println!(
      "cargo:warning=this often happens because the target binary isn't built \
       yet, try running `cargo build` again"
    );
    println!(
      "cargo:warning=keep in mind that this is for development purposes only"
    );
  }

  Ok(())
}
