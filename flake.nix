{
  description = "Settings daemon for the COSMIC desktop environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    nixpkgs-kora.url = "github:NixOS/nixpkgs/b7c2ada94fe99c15b0dbcf4d11fd7850b957a436";
    flake-utils.url = "github:numtide/flake-utils";
    nix-filter.url = "github:numtide/nix-filter";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, nixpkgs-kora, flake-utils, nix-filter, crane, fenix }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # The shell's gcc and glibc decide which symbol versions the binary asks
        # for, so `deploy:nix` packages against this stdenv.
        pkgsKora = import nixpkgs-kora { inherit system; };
        craneLib = crane.lib.${system}.overrideToolchain fenix.packages.${system}.stable.toolchain;

        pkgDef = {
          src = nix-filter.lib.filter {
            root = ./.;
            include = [
              ./src
              ./Cargo.toml
              ./Cargo.lock
            ];
          };
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [
            systemd # For libudev
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly pkgDef;
        cosmic-settings-daemon = craneLib.buildPackage (pkgDef // {
          inherit cargoArtifacts;
        });
      in {
        checks = {
          inherit cosmic-settings-daemon;
        };

        packages.default = cosmic-settings-daemon;

        apps.default = flake-utils.lib.mkApp {
          drv = cosmic-settings-daemon;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = builtins.attrValues self.checks.${system};
        };

        devShells.deploy = pkgsKora.mkShell {
          name = "cosmic-settings-daemon-deploy";

          nativeBuildInputs = with pkgsKora; [
            rustc
            cargo
            pkg-config
            cmake
            rustPlatform.bindgenHook # clang-sys: sets LIBCLANG_PATH
            go-task # the inner `task dist:rpm`
            rpm # rpmbuild
          ];

          buildInputs = with pkgsKora; [
            systemd # libudev-sys
            libinput # input-sys
            libxkbcommon # wayland-sys
            wayland
            openssl # openssl-sys
            libpulseaudio
            pipewire # pipewire-sys / libspa-sys
            wireplumber
            libdrm # drm-sys
          ];

          # dlopen'd, so they are on no link line.
          LD_LIBRARY_PATH = pkgsKora.lib.makeLibraryPath (
            with pkgsKora;
            [
              wayland
              libxkbcommon
            ]
          );
        };
      });

  nixConfig = {
    # Cache for the Rust toolchain in fenix
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [ "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=" ];
  };
}
