{
  mkShell,
  rustPlatform,
  taplo,
  cargo,
  rustc,
  clang,
  clippy,
  lldb,
  rust-analyzer-unwrapped,
  rustfmt,
  cargo-nextest,
  dbus,
}: let
  inherit (rustc) llvmPackages;
in
  mkShell {
    name = "watt-dev";
    packages = [
      taplo # TOML formatter

      # Build tool
      cargo
      rustc
      clang
      llvmPackages.lld # linker

      # Tooling
      clippy # lints
      lldb # debugger
      rust-analyzer-unwrapped # LSP
      (rustfmt.override {asNightly = true;}) # formatter

      # Additional Cargo utils
      cargo-nextest
      dbus
    ];

    env = {
      RUST_SRC_PATH = rustPlatform.rustLibSrc;

      LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
      RUSTFLAGS = "-C linker=clang -C link-arg=-fuse-ld=lld";
    };
  }
