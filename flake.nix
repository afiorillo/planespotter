{
  description = "planespotter — a monitor for nearby airplanes";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    # Per-system outputs (packages, devShells, apps).
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        
        planespotter = pkgs.rustPlatform.buildRustPackage {
          pname = "planespotter";
          version = "0.1.0";

          src = self;
          cargoLock.lockFile = ./Cargo.lock;

          # `pkg-config` locates `openssl`, needed by reqwest's native-tls (pulled
          # in by the default `steam-api` feature).
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];

          meta = with pkgs.lib; {
            description = "A monitor for nearby planes";
            homepage = "https://github.com/afiorillo/planespotter";
            license = licenses.agpl3Plus;
            mainProgram = "planespotter";
            platforms = platforms.unix ++ platforms.windows;
          };
        };
      in
      {
        packages = {
          default = planespotter;
          planespotter = planespotter;
        };

        apps.default = flake-utils.lib.mkApp { drv = planespotter; };

        devShells.default = pkgs.mkShell {
          # Pull in the package's build inputs (openssl, pkg-config, …) so the
          # dev shell can build it, plus the interactive toolchain.
          inputsFrom = [ planespotter ];
          packages = [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.rustfmt
            pkgs.gcc
            pkgs.git
          ];
          env.CC = "cc";
        };

        formatter = pkgs.nixfmt;
      }
    )
    # System-independent outputs: overlay + modules.
    // {
      overlays.default = final: prev: {
        planespotter = self.packages.${final.system}.default;
      };
    };
}
