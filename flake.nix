{
  description = "Lumen — a Wayland compositor that streams the desktop to browsers via WebRTC";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # crane intentionally has no nixpkgs input; it binds to whatever pkgs
    # instance is passed to `crane.mkLib`.
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      ...
    }:
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
          lumen = pkgs.callPackage ./nix/package.nix {
            craneLib = crane.mkLib pkgs;
          };
          default = self.packages.${system}.lumen;
        }
      );

      overlays.default = final: prev: {
        lumen = prev.callPackage ./nix/package.nix {
          craneLib = crane.mkLib prev;
        };
      };

      nixosModules = {
        # Close over `crane` so the module can build the package with the
        # consumer's pkgs without requiring them to add the input themselves.
        lumen = import ./nix/module.nix { inherit crane; };
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
