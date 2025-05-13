{
  mkShell,
  rust-analyzer-unwrapped,
  rustfmt,
  clippy,
  cargo,
  rustc,
  rustPlatform,
}:
mkShell {
  packages = [
    cargo
    clippy
    rustc
    rustfmt
    rust-analyzer-unwrapped
  ];

  env.RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
