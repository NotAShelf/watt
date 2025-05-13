{
  description = "Superfreq";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    systems.url = "github:nix-systems/default-linux";
  };

  outputs = inputs @ {
    self,
    nixpkgs,
    systems,
  }: let
    inherit (nixpkgs) lib;
    eachSystem = lib.genAttrs (import inputs.systems);
    pkgsFor = eachSystem (system: nixpkgs.legacyPackages.${system});
  in {
    devShells = eachSystem (system: {
      default = pkgsFor.${system}.callPackage ./nix/shell.nix {};
    });
  };
}
