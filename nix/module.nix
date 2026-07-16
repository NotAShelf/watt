{
  config,
  pkgs,
  lib,
  ...
}: let
  inherit (lib) types;
  inherit (lib.modules) mkIf;
  inherit (lib.options) mkOption mkEnableOption mkPackageOption;
  inherit (lib.meta) getExe;

  cfg = config.services.watt;

  format = pkgs.formats.toml {};
  cfgFile = format.generate "watt-config.toml" cfg.settings;

  wattPackage = pkgs.callPackage ./package.nix {};

  conflictingServices = [
    "power-profiles-daemon"
    "auto-cpufreq"
    "tlp"
    "cpupower-gui"
    "thermald"
    "tuned"
  ];

  configString = lib.mkMerge [
    (mkIf cfg.configFile != null cfg.configFile)
    (mkIf (cfg.configFile == null) cfg.settings)
  ];

  wrapper = pkgs.stdenvNoCC.mkDerivation {
    name = "watt-wrapped";
    buildInputs = [pkgs.makeWrapper];
    paths = [cfg.package];
    meta.mainProgram = "watt";
    preferLocalBuild = true;
    allowSubstitutes = false;
    enableParallelBuilding = true;

    buildPhase = ''
      mkdir -p $out
      wrapProgram $out/bin/watt --set WATT_CONFIG ${configString}
    '';
  };
in {
  options.services.watt = {
    enable = mkEnableOption "Watt, automatic CPU speed & power optimizer for Linux";
    package = mkPackageOption wattPackage "watt" {
      pkgsText = "self.packages.\${pkgs.stdenv.hostPlatform.system}";
    };

    settings = mkOption {
      type = types.submodule {freeformType = format.type;};
      default = {};
      description = ''
        Configuration for Watt.
        Disjoint with `configFile` option.
      '';
    };

    configFile = mkOption {
      type = types.nullOr (types.either types.package types.str);
      default = null;
      description = ''
        Configuration for Watt.
        Disjoint with `configFile` option.
      '';
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    services.dbus.packages = [cfg.package];

    systemd.services.watt = {
      wantedBy = ["multi-user.target"];
      conflicts = map (service: "${service}.service") conflictingServices;
      serviceConfig = {
        WorkingDirectory = "";
        ExecStart = getExe wrapper;
        Restart = "on-failure";

        RuntimeDirectory = "watt";
        RuntimeDirectoryMode = "0755";
      };
    };

    assertions =
      map (service: {
        assertion = !config.services.${service}.enable;
        message = "You have set services.${service}.enable = true; which conflicts with Watt.";
      })
      conflictingServices;
  };
}
