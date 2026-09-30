{
  description = "Lumen — a Wayland compositor that streams the desktop to browsers via WebRTC";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          lumen = pkgs.callPackage ./nix/package.nix { };
          default = self.packages.${system}.lumen;
        }
      );

      overlays.default = final: prev: {
        lumen = prev.callPackage ./nix/package.nix { };
      };

      nixosModules = {
        lumen = import ./nix/module.nix;
        default = self.nixosModules.lumen;
      };

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = import ./nix/shell.nix { inherit pkgs; };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-rfc-style);

      checks = forAllSystems (system: {
        lumen = self.packages.${system}.lumen;
      });
    };
}
