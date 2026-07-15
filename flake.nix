{
  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";

  outputs = {
    self,
    nixpkgs,
    ...
  } @ inputs: let
    forAllSystems = nixpkgs.lib.genAttrs ["x86_64-linux" "aarch64-linux"];
    pkgsForEach = forAllSystems (system: nixpkgs.legacyPackages.${system});
  in {
    overlays = {
      watt = final: _: {
        watt = final.callPackage ./nix/package.nix {};
      };
      default = self.overlays.watt;
    };

    packages =
      nixpkgs.lib.mapAttrs (system: pkgs: {
        watt = pkgs.callPackage ./nix/package.nix {};
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
