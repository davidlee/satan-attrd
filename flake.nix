{
  description = "satan-attrd — SATAN attribute layer daemon";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devshell.url = "github:numtide/devshell";
    agents = {
      url = "github:davidlee/nix-config?dir=flakes/agents";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    spec-driver.url = "github:davidlee/spec-driver";
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
        inherit (pkgs) lib stdenv;
        isLinux = stdenv.hostPlatform.isLinux;

        jailLib =
          if isLinux
          then inputs.agents.lib.${system}.mkJailedAgents {}
          else {};

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
          supabase-cli
          spec-driver-pkg
        ];
        projectPkgs = devToolPkgs ++ [(jailLib.unjailed.codex or pkgs.codex)];

        # Environment forwarded into jail via bwrap --setenv
        jailEnvOptions = with jailLib.combinators; [
          (try-fwd-env "DATABASE_URL")
          (try-fwd-env "DOCKER_HOST")
          (try-readwrite "/run/user/1000/docker.sock")
          (set-env "LD_LIBRARY_PATH" "${lib.makeLibraryPath [pkgs.stdenv.cc.cc.lib]}")
        ];

        workspaceDeps = ["/home/david/.config/emacs/"];

        # Every jail reaches models through the local LLM gateway with the
        # shared `pi` key (~/.config/llm-gateway/keys/pi); no provider key
        # enters a jail.
        gateway = jailLib.gatewayClient "pi";

        jailPkgs = lib.optionalAttrs isLinux {
          jailed-pi = jailLib.makeJailedPi {
            profile = "specDev";
            exposePostgres = true;
            allowSelfAsSubagent = true;
            maxSubagentDepth = 2;
            extraPkgs = projectPkgs;
            extraOptions = jailEnvOptions;
            inherit gateway workspaceDeps;
          };
          jailed-pi-research = jailLib.makeJailedPi {
            name = "pi-research";
            profile = "research";
            extraPkgs = projectPkgs;
            extraOptions = jailEnvOptions;
            inherit gateway workspaceDeps;
          };
          # jailed-opencode = jailLib.makeJailedOpencode {
          #   profile = "specDev";
          #   extraPkgs = projectPkgs;
          #   extraOptions = jailEnvOptions;
          #   inherit workspaceDeps;
          # };
          jailed-claude = jailLib.makeJailedClaude {
            profile = "specDev";
            extraPkgs = projectPkgs;
            extraOptions = jailEnvOptions;
            inherit gateway workspaceDeps;
          };
          jailed-codex = jailLib.makeJailedCodex {
            profile = "specDev";
            extraPkgs = projectPkgs;
            extraOptions = jailEnvOptions;
            inherit gateway workspaceDeps;
          };
          #jailed-gemini = jailLib.makeJailedGemini {
          #  profile = "specDev";
          #  extraPkgs = projectPkgs;
          #  extraOptions = jailEnvOptions;
          #  inherit workspaceDeps;
          #};
          bubblewrap = pkgs.bubblewrap;
        };

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

        packages =
          jailPkgs
          // {
            inherit satan-attrd;
            default = satan-attrd;
            # The devshell's tool set as one closure, so the VM and the devshell
            # can't drift.
            dev-tools = pkgs.buildEnv {
              name = "satan-attrd-dev-tools";
              paths = devToolPkgs;
              ignoreCollisions = true;
            };
          };

        devshells.default = {
          packages = projectPkgs ++ lib.optionals isLinux (lib.attrValues jailPkgs);

          env = [
            {
              name = "LD_LIBRARY_PATH";
              value = lib.makeLibraryPath [pkgs.stdenv.cc.cc.lib];
            }
          ];

          commands = [
            {
              name = "sdr";
              help = "spec-driver";
              command = "spec-driver $@";
            }
            {
              name = "jpi";
              help = "jailed-pi";
              command = "jailed-pi $@";
            }
            {
              name = "jcl";
              help = "jailed-claude --dangerously-skip-permissions";
              command = "jailed-claude --dangerously-skip-permissions $@";
            }
          ];
        };
      };
    };
}
