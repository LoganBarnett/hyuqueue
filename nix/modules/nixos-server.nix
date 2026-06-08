# NixOS (Linux/systemd) module for the hyuqueue-server service.
# Thin wrapper around the foundation's mkNixosService helper.  See
# `darwin-server.nix` for the macOS/launchd equivalent.
# Cross-platform declarations live in `./common.nix`, which is
# imported below so anything added there merges into both platform
# modules via the module-merge system.
#
# This file is the seam where NixOS-only declarations belong: the
# `hyuqueue_server_config` env var that points the server at the
# generated `config.toml`, and the `ReadWritePaths` entry that
# lets the SQLite directory escape the foundation's
# `ProtectSystem=strict` hardening.
#
# The config-file path is injected via an environment variable
# (`hyuqueue_server_config`, the macro-derived env name for
# `--config`) rather than by overriding `ExecStart`.  Overriding
# ExecStart would also drop the foundation's `--listen sd-listen`
# / `--frontend-path` flag construction and any future additions
# the foundation makes to the command line; the env-var injection
# leaves all of that intact.
#
# Upstream TODO: a `configFile` option on `mkNixosService` would
# let the foundation own this wiring, leaving this file with
# essentially just the `ReadWritePaths` line.  See the
# rust-template task "Add configFile option to mkNixosService and
# mkDarwinService".
#
# Minimal usage (defaults to Unix domain socket):
#
#   imports = [ inputs.hyuqueue.nixosModules.server ];
#   services.hyuqueue-server = {
#     enable = true;
#     baseUrl = "https://hyuqueue.example.com";
#   };
#
# Topics:
#
#   services.hyuqueue-server.topics."rss-tech" = {
#     command = [ "${pkgs.hyuqueue-topic-rss}/bin/hyuqueue-topic-rss" ];
#     settings.feeds = [ "https://example.com/feed" ];
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
    ...
  }: let
    cfg = config.services.${name};
  in {
    imports = [
      ./common.nix
      (foundation.lib.mkNixosService {inherit name self;})
    ];

    config = lib.mkIf cfg.enable {
      systemd.services.${name} = {
        # Macro-derived env var that the server's clap reads as
        # the `--config` path.
        environment.hyuqueue_server_config =
          toString cfg._generatedConfigFile;

        # The SQLite directory lives outside the Nix store; the
        # foundation's `ProtectSystem=strict` hardening requires
        # an explicit ReadWritePaths entry for it.
        serviceConfig.ReadWritePaths = [(dirOf cfg.dbPath)];
      };
    };
  }
