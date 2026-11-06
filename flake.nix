{
  description = "satan-attrd — SATAN attribute layer daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devshell.url = "github:numtide/devshell";
    spec-driver.url = "github:davidlee/spec-driver";
    doctrine.url = "github:davidlee/doctrine";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = inputs @ {
    flake-parts,
    spec-driver,
    rust-overlay,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [
        inputs.devshell.flakeModule
      ];

      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

      flake = {
        homeManagerModules.default = import ./nix/module.nix;
        homeManagerModules.satan-attrd = import ./nix/module.nix;
      };

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        inherit (pkgs) lib;

        spec-driver-pkg = spec-driver.packages.${system}.default;

        # The tool set minus the agents. Exported as `packages.dev-tools` for
        # consumers that confine differently — the oubliette microvm capsule's
        # `toolsPackage`, where the agents come from the capsule.
        devToolPkgs = with pkgs; [
          just
          postgresql_18

          rust-bin.stable.latest.default

          stdenv.cc # cc/ld on PATH (linker for cargo build)
          stdenv.cc.cc.lib
          spec-driver-pkg
        ];

        # Rust binary
        satan-attrd = pkgs.rustPlatform.buildRustPackage {
          pname = "satan-attrd";
          version = "0.1.0";
          src = lib.cleanSourceWith {
            src = ./.;
            filter = path: type: let
              baseName = builtins.baseNameOf path;
              relPath = lib.removePrefix (toString ./. + "/") (toString path);
            in
              # Include single-crate sources + migrations baked into the binary
              # by sqlx::migrate! at build time.
              (
                type
                == "directory"
                && builtins.elem baseName [
                  "src"
                  "tests"
                  "migrations"
                ]
              )
              || baseName == "Cargo.toml"
              || baseName == "Cargo.lock"
              || lib.hasPrefix "src/" relPath
              || lib.hasPrefix "tests/" relPath
              || lib.hasPrefix "migrations/" relPath;
          };
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false; # tests require a live Postgres
          meta = {
            mainProgram = "satan-attrd";
            description = "SATAN attribute layer daemon";
          };
        };
      in {
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [rust-overlay.overlays.default];
        };

        packages = {
          inherit satan-attrd;
          default = satan-attrd;
          # The devshell's tool set as one closure, so the VM and the devshell
          # can't drift.
          dev-tools = pkgs.buildEnv {
            name = "satan-attrd-dev-tools";
            paths = devToolPkgs;
            ignoreCollisions = true;
            # The dev-envs consumer's per-project env (flakes:SL-022).
            passthru.devEnv.env.LD_LIBRARY_PATH = lib.makeLibraryPath [pkgs.stdenv.cc.cc.lib];
          };
          # What the room takes: `dev-tools` plus Doctrine, as a room has no host install
          # (flakes:DEC-130).
          capsule-tools = pkgs.buildEnv {
            name = "satan-attrd-capsule-tools";
            paths = devToolPkgs ++ [inputs.doctrine.packages.${system}.default];
            ignoreCollisions = true;
          };
        };

        devshells.default = {
          packages = devToolPkgs;

          env = [
            {
              name = "LD_LIBRARY_PATH";
              value = lib.makeLibraryPath [pkgs.stdenv.cc.cc.lib];
            }
          ];
        };
      };
    };
}
