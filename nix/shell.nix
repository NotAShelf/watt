{
  mkShell,
  cargo,
  rustc,
  rustfmt,
  clippy,
  rust-analyzer-unwrapped,
  rustPlatform,
}:
mkShell {
  packages = [
    cargo
    rustc
    clippy # lints
    (rustfmt.override {asNightly = true;})
    rust-analyzer-unwrapped
  ];

  env.RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
