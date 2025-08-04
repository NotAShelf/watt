{
  mkShell,
  cargo,
  rustc,
  rustfmt,
  clippy,
  rust-analyzer-unwrapped,
  taplo,
  rustPlatform,
}:
mkShell {
  packages = [
    cargo
    rustc
    clippy # lints
    (rustfmt.override {asNightly = true;})
    rust-analyzer-unwrapped

    # TOML formatter
    taplo
  ];

  env.RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
