# Darwin (macOS/launchd) module for the hyuqueue-server service.
# Thin wrapper around the foundation's mkDarwinService helper.  See
# `nixos-server.nix` for the Linux/systemd equivalent.
# Cross-platform declarations live in `./common.nix`, which is
# imported below so anything added there merges into both platform
# modules via the module-merge system.
#
# This file is the seam where nix-darwin-only declarations belong:
# the `hyuqueue_server_config` env var that points the server at
# the generated `config.toml`, and an activation script ensuring
# the SQLite directory exists.
#
# The config-file path is injected via an environment variable
# (`hyuqueue_server_config`, the macro-derived env name for
# `--config`) rather than by overriding `ProgramArguments`.  The
# foundation's `ProgramArguments` already does the work that
# matters on Darwin — creating, chowning, and chmoding the socket
# directory before sudo-dropping to the service user — and an
# override would silently lose it.  Env-var injection leaves the
# foundation's launch script intact.
#
# Upstream TODO: a `configFile` option on `mkDarwinService` would
# let the foundation own this wiring, leaving this file with
# essentially just the activation script for the db dir.  See
# the rust-template task "Add configFile option to mkNixosService
# and mkDarwinService".
#
# Minimal usage (defaults to Unix domain socket):
#
#   imports = [ inputs.hyuqueue.darwinModules.server ];
#   services.hyuqueue-server = {
#     enable = true;
#     baseUrl = "https://hyuqueue.example.com";
#   };
{
  self,
  foundation,
}: let
  name = "hyuqueue-server";
in
  {
    config,
    lib,
    pkgs,
    ...
  }: let
    cfg = config.services.${name};
  in {
    imports = [
      ./common.nix
      (foundation.lib.mkDarwinService {inherit name self;})
    ];

    config = lib.mkIf cfg.enable {
      # Ensure the SQLite directory exists at activation.  The
      # foundation creates /var/log/<name>; this adds the db dir
      # using the same `${pkgs.coreutils}/bin/mkdir` invocation
      # style (BSD mkdir lacks --parents).
      system.activationScripts.postActivation.text = ''
        ${pkgs.coreutils}/bin/mkdir --parents ${dirOf cfg.dbPath}
        chown ${cfg.user}:${cfg.group} ${dirOf cfg.dbPath}
        chmod 0750 ${dirOf cfg.dbPath}
      '';

      # Macro-derived env var that the server's clap reads as the
      # `--config` path.
      launchd.servers.${name}.serviceConfig.EnvironmentVariables.hyuqueue_server_config =
        toString cfg._generatedConfigFile;
    };
  }
