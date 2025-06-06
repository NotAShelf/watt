inputs: {
  config,
  pkgs,
  lib,
  ...
}: let
  inherit (lib.modules) mkIf;
  inherit (lib.options) mkOption mkEnableOption mkPackageOption;
  inherit (lib.types) submodule;
  inherit (lib.lists) optional;
  inherit (lib.meta) getExe;

  cfg = config.services.superfreq;

  format = pkgs.formats.toml {};
  cfgFile = format.generate "superfreq-config.toml" cfg.settings;
in {
  options.services.superfreq = {
    enable = mkEnableOption "Automatic CPU speed & power optimizer for Linux";
    package = mkPackageOption inputs.self.packages.${pkgs.stdenv.system} "superfreq" {
      pkgsText = "self.packages.\${pkgs.stdenv.system}";
    };

    settings = mkOption {
      default = {};
      type = submodule {freeformType = format.type;};
      description = "Configuration for Superfreq.";
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [cfg.package];

    # This is necessary for the Superfreq CLI. The environment variable
    # passed to the systemd service will take priority in read order.
    environment.etc."superfreq.toml".source = cfgFile;

    systemd.services.superfreq = {
      wantedBy = ["multi-user.target"];
      conflicts = [
        "auto-cpufreq.service"
        "power-profiles-daemon.service"
        "tlp.service"
        "cpupower-gui.service"
        "thermald.service"
      ];
      serviceConfig = {
        Environment = optional (cfg.settings != {}) ["SUPERFREQ_CONFIG=${cfgFile}"];
        WorkingDirectory = "";
        ExecStart = "${getExe cfg.package} daemon --verbose";
        Restart = "on-failure";

        RuntimeDirectory = "superfreq";
        RuntimeDirectoryMode = "0755";
      };
    };

    assertions = [
      {
        assertion = !config.services.power-profiles-daemon.enable;
        message = ''
          You have set services.power-profiles-daemon.enable = true;
          which conflicts with Superfreq.
        '';
      }
      {
        assertion = !config.services.auto-cpufreq.enable;
        message = ''
          You have set services.auto-cpufreq.enable = true;
          which conflicts with Superfreq.
        '';
      }
    ];
  };
}
