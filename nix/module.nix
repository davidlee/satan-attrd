# Home-Manager module exporting the satan-attrd systemd user service.
#
# Wire-up from a flake using flake-parts + import-tree:
#
#   inputs.satan-attrd.url = "path:/home/david/dev/satan-attrd";
#   inputs.satan-attrd.inputs.nixpkgs.follows = "nixpkgs-home";
#
#   # in modules/home/satan-attrd.nix:
#   _: {
#     flake.homeModules.satan-attrd = {inputs, pkgs, ...}: {
#       imports = [ inputs.satan-attrd.homeManagerModules.default ];
#       services.satan-attrd = {
#         enable = true;
#         package = inputs.satan-attrd.packages.${pkgs.system}.satan-attrd;
#       };
#     };
#   }
#
# Then add `homeModules.satan-attrd` to the host's Sleipnir.nix imports.
#
# Migrations are NOT auto-applied. Run `satan-attrd migrate` by hand
# (design-contract §17 — explicit migrate, no boot-time sqlx::migrate!).
#
# Smoke:
#   systemctl --user status satan-attrd
#   journalctl --user -u satan-attrd -f
#   psql -d satan_memory -c 'SELECT count(*) FROM satan_outcome_inbox;'
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.satan-attrd;
in {
  options.services.satan-attrd = {
    enable = lib.mkEnableOption "satan-attrd attribute daemon";

    package = lib.mkOption {
      type = lib.types.package;
      description = "satan-attrd package to use.";
    };

    databaseUrl = lib.mkOption {
      type = lib.types.str;
      default = "postgres:///satan_memory?host=/run/postgresql";
      description = "Postgres DSN passed via DATABASE_URL.";
    };

    rustLog = lib.mkOption {
      type = lib.types.str;
      default = "satan_attrd=info";
      description = "RUST_LOG env-filter directive.";
    };

    extraEnvironment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = {};
      description = "Additional Environment= entries for the systemd unit.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Broker (~/.emacs.d satan listeners) needs `satan-attrd` on PATH
    # so the `notify-stream` subprocess can be spawned via executable-find.
    home.packages = [cfg.package];

    systemd.user.services.satan-attrd = {
      Unit = {
        Description = "satan-attrd: SATAN attribute daemon";
        After = ["graphical-session.target"];
      };
      Service =
        {
          Type = "simple";
          Environment =
            [
              "DATABASE_URL=${cfg.databaseUrl}"
              "RUST_LOG=${cfg.rustLog}"
              "PATH=${lib.makeBinPath [pkgs.coreutils]}"
            ]
            ++ lib.mapAttrsToList (k: v: "${k}=${v}") cfg.extraEnvironment;
          ExecStart = "${cfg.package}/bin/satan-attrd run";
          Restart = "on-failure";
          RestartSec = "5s";
          # Don't compete with interactive work — the daemon is a
          # background consumer that can yield.
          Nice = 15;
          IOSchedulingClass = "idle";
        };
      Install.WantedBy = ["default.target"];
    };
  };
}
