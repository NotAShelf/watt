{
  lib,
  stdenv,
  rustPlatform,
  versionCheckHook,
}: let
  fs = lib.fileset;
in
  rustPlatform.buildRustPackage (finalAttrs: {
    pname = "watt";
    version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).workspace.package.version;

    src = fs.toSource {
      root = ../.;
      fileset = fs.unions [
        ../.cargo
        ../watt
        ../xtask
        ../Cargo.lock
        ../Cargo.toml
      ];
    };

    cargoLock.lockFile = "${finalAttrs.src}/Cargo.lock";
    useFetchCargoVendor = true;
    enableParallelBuilding = true;

    # xtask doesn't support passing --targe
    # but nix hooks expect the folder structure from when it's set
    env.CARGO_BUILD_TARGET = stdenv.hostPlatform.rust.cargoShortTarget;

    nativeInstallCheckInputs = [versionCheckHook];
    versionCheckProgram = "${placeholder "out"}/bin/${finalAttrs.meta.mainProgram}";
    versionCheckProgramArg = "--version";
    doInstallCheck = true;

    postInstall = ''
      # Install required files with the 'dist' task
      cargo xtask dist \
        --completions-dir $out/share/completions \
        --bin-dir $out/bin \
        --watt-binary $out/bin/watt
    '';

    meta = {
      description = "Automatic CPU speed & power optimizer for Linux";
      longDescription = ''
        Watt is a CPU speed & power optimizer for Linux. It uses
        the CPU frequency scaling driver to set the CPU frequency
        governor and the CPU power management driver to set the CPU
        power management mode.
      '';
      homepage = "https://github.com/NotAShelf/watt";
      mainProgram = "watt";
      maintainers = [lib.maintainers.NotAShelf];
      license = lib.licenses.mpl20;
      platforms = lib.platforms.linux;
    };
  })
