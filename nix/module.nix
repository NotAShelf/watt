inputs: {
  config,
  pkgs,
  lib,
  ...
}: let
  inherit (lib.modules) mkIf mkDefault;
  inherit (lib.options) mkOption mkEnableOption mkPackageOption;
  inherit (lib.types) submodule;
  inherit (lib.meta) getExe;

  cfg = config.services.watt;

  format = pkgs.formats.toml {};
  cfgFile = format.generate "watt-config.toml" cfg.settings;

  conflictingServices = [
    "power-profiles-daemon"
    "auto-cpufreq"
    "tlp"
    "cpupower-gui"
    "thermald"
    "tuned"
  ];
in {
  options.services.watt = {
    enable = mkEnableOption "Watt, automatic CPU speed & power optimizer for Linux";
    package = mkPackageOption inputs.self.packages.${pkgs.stdenv.hostPlatform.system} "watt" {
      pkgsText = "self.packages.\${pkgs.stdenv.hostPlatform.system}";
    };

    settings = mkOption {
      type = submodule {freeformType = format.type;};
      default = {};
      description = "Configuration for Watt.";
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    services.dbus.packages = [cfg.package];
    security.polkit.enable = mkDefault true;

    environment.etc."watt.toml".source = cfgFile;

    systemd.services.watt = {
      wantedBy = ["multi-user.target"];
      conflicts = map (service: "${service}.service") conflictingServices;
      serviceConfig = {
        WorkingDirectory = "";
        ExecStart = getExe cfg.package;
        Restart = "on-failure";

        RuntimeDirectory = "watt";
        RuntimeDirectoryMode = "0755";
        StateDirectory = "watt";
        Environment = [
          "WATT_CONFIG=/etc/watt.toml"
          "WATT_STATE_PATH=%S/watt/profile.toml"
        ];
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
