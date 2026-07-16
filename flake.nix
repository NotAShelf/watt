{
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";

  outputs =
    {
      self,
      nixpkgs,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems =
        set:
        builtins.listToAttrs (
          map (system: {
            name = system;
            value = set nixpkgs.legacyPackages.${system} system;
          }) systems
        );
    in
    {
      overlays = {
        watt = final: _: {
          watt = final.callPackage ./nix/package.nix { };
        };
        default = self.overlays.watt;
      };

      packages = forAllSystems (
        pkgs: system: {
          watt = pkgs.callPackage ./nix/package.nix { };
          default = self.packages.${system}.watt;
        }
      );

      devShells = forAllSystems (pkgs: system: {
        default = pkgs.callPackage ./nix/shell.nix { };
      });

      nixosModules = {
        watt = import ./nix/module.nix;
        default = self.nixosModules.watt;
      };

      formatter = forAllSystems (pkgs: _: pkgs.alejandra);
    };
}
