{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs?ref=nixos-unstable";

  outputs = {
    self,
    nixpkgs,
    ...
  } @ inputs: let
    forAllSystems = nixpkgs.lib.genAttrs ["x86_64-linux" "aarch64-linux"];
    pkgsForEach = forAllSystems (system:
      import nixpkgs {
        localSystem.system = system;
        overlays = [self.overlays.default];
      });
    forAllDevSystems = nixpkgs.lib.genAttrs ["x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin"];
    pkgsForEachDev = forAllDevSystems (system:
      import nixpkgs {
        localSystem.system = system;
        overlays = [self.overlays.default];
      });
  in {
    overlays = {
      watt = final: _: {
        watt = final.callPackage ./nix/package.nix {};
      };
      default = self.overlays.watt;
    };

    packages =
      nixpkgs.lib.mapAttrs (system: pkgs: {
        inherit (pkgs) watt;
        default = self.packages.${system}.watt;
      })
      pkgsForEach;

    devShells =
      nixpkgs.lib.mapAttrs (system: pkgs: {
        default = pkgs.callPackage ./nix/shell.nix {};
      })
      pkgsForEachDev;

    nixosModules = {
      watt = import ./nix/module.nix inputs;
      default = self.nixosModules.watt;
    };

    formatter = forAllDevSystems (system: nixpkgs.legacyPackages.${system}.alejandra);
  };
}
