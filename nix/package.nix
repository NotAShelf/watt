{
  lib,
  stdenv,
  rustPlatform,
  versionCheckHook,
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "watt";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;
  __structuredAttrs = true;

  src = let
    fs = lib.fileset;
    s = ../.;
  in
    fs.toSource {
      root = s;
      fileset = fs.unions [
        ../.cargo
        ../watt
        ../xtask
        ../Cargo.lock
        ../Cargo.toml
      ];
    };

  cargoBuildFlags = ["-p watt" "-p xtask"];
  cargoLock.lockFile = "${finalAttrs.src}/Cargo.lock";
  enableParallelBuilding = true;
  useNextest = true;

  # xtask doesn't support passing --target
  # but nix hooks expect the folder structure from when it's set
  env.CARGO_BUILD_TARGET = stdenv.hostPlatform.rust.cargoShortTarget;

  nativeInstallCheckInputs = [versionCheckHook];
  versionCheckProgram = "${placeholder "out"}/bin/${finalAttrs.meta.mainProgram}";
  versionCheckProgramArg = "--version";
  doInstallCheck = true;

  postInstall =
    lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
      # Install required files with the 'dist' task
      $out/bin/xtask dist --completions-dir $out/share/completions
    ''
    + ''
      # Avoid populating PATH with an 'xtask' cmd
      rm $out/bin/xtask

      # Install Watt's dbus
      install -Dm644 dbus/net.hadess.PowerProfiles.conf \
        $out/share/dbus-1/system.d/net.hadess.PowerProfiles.conf
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
