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
      superfreq = final: _: {
        superfreq = final.callPackage ./nix/package.nix {};
      };
      default = self.overlays.superfreq;
    };

    packages =
      nixpkgs.lib.mapAttrs (system: pkgs: {
        inherit (pkgs) superfreq;
        default = self.packages.${system}.superfreq;
      })
      pkgsForEach;

    devShells =
      nixpkgs.lib.mapAttrs (system: pkgs: {
        default = pkgs.callPackage ./nix/shell.nix {};
      })
      pkgsForEach;

    nixosModules = {
      superfreq = import ./nix/module.nix inputs;
      default = self.nixosModules.superfreq;
    };

    formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.alejandra);
  };
}
