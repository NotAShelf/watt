{
  lib,
  rustPlatform,
}: let
  fs = lib.fileset;
in
  rustPlatform.buildRustPackage (finalAttrs: {
    pname = "superfreq";
    version = "0.1.0";

    src = fs.toSource {
      root = ../.;
      fileset = fs.unions [
        (fs.fileFilter (file: builtins.any file.hasExt ["rs"]) ../src)
        ../Cargo.lock
        ../Cargo.toml
      ];
    };

    cargoLock.lockFile = "${finalAttrs.src}/Cargo.lock";
    useFetchCargoVendor = true;
    enableParallelBuilding = true;

    meta = {
      description = "Automatic CPU speed & power optimizer for Linux";
      longDescription = ''
        Superfreq is a CPU speed & power optimizer for Linux. It uses
        the CPU frequency scaling driver to set the CPU frequency
        governor and the CPU power management driver to set the CPU
        power management mode.

      '';
      homepage = "https://github.com/NotAShelf/superfreq";
      mainProgram = "superfreq";
      license = lib.licenses.mpl20;
      platforms = lib.platforms.linux;
    };
  })
