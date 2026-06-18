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

          # TLS is provided by rustls (reqwest is built with `default-features = false`
          # and the `rustls-tls` feature), so no OpenSSL/pkg-config is required — only a
          # C compiler for linking, which buildRustPackage's stdenv supplies.
          nativeBuildInputs = [ ];
          buildInputs = [ ];

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
          # Self-contained interactive toolchain. TLS uses rustls, so the only native
          # requirement beyond the Rust toolchain is a C compiler for linking (gcc).
          # Kept independent of the `planespotter` derivation so `nix develop` works
          # before Cargo.lock is committed.
          packages = [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.gcc
            pkgs.git
          ];
          env.CC = "cc";
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
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
