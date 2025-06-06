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
      pkgsForEach;

    nixosModules = {
      watt = import ./nix/module.nix inputs;
      default = self.nixosModules.watt;
    };

    formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.alejandra);
  };
}
