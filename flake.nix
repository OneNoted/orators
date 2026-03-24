{
  description = "Orators Bluetooth desktop speaker utility";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, flake-utils, nixpkgs }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "orators";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "orators" ];
          cargoTestFlags = [ "-p" "orators" ];
        };

        checks.default = self.packages.${system}.default;

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            bluez
            cargo
            clippy
            dbus
            nixfmt-rfc-style
            pkg-config
            pipewire
            rustc
            rustfmt
            wireplumber
          ];
        };
      });
}

