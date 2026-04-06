# NixOS (Linux/systemd) module for the hyuqueue-server service.
# Exported from the flake as nixosModules.server.
# See darwin-server.nix for the macOS/launchd equivalent.
#
# Minimal usage (defaults to Unix domain socket):
#
#   inputs.hyuqueue.nixosModules.server
#
#   services.hyuqueue-server = {
#     enable = true;
#   };
#
# To use TCP instead:
#
#   services.hyuqueue-server = {
#     enable = true;
#     socket = null;
#     port   = 8731;
#   };
#
# To reference the socket from a reverse proxy (e.g. nginx):
#
#   locations."/".proxyPass =
#     "http://unix:${config.services.hyuqueue-server.socket}";
#
# Note: when using socket mode the reverse proxy user must be a member of
# the service group (cfg.group) so it can connect to the socket.
{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.hyuqueue-server;
in {
  options.services.hyuqueue-server = {
    enable = lib.mkEnableOption "hyuqueue-server service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.server;
      defaultText = lib.literalExpression "self.packages.\${system}.server";
      description = "Package providing the service binary.";
    };

    socket = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = "/run/hyuqueue-server/hyuqueue-server.sock";
      description = ''
        Path for the Unix domain socket used by the service.  When set,
        systemd socket activation is used and the host/port options are
        ignored.  Set to null to use TCP instead.

        Other services (e.g. nginx) that proxy to this socket must be
        members of the service group to connect.
      '';
    };

    # host and port are separate options (rather than a single "listen"
    # string) so that other Nix expressions can reference them
    # individually — e.g. firewall rules need the port, reverse proxy
    # configs need host:port, and health-check URLs need both.  The
    # module combines them into the --listen flag internally.
    host = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "IP address to bind to.  Ignored when socket is set.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8731;
      description = "TCP port to listen on.  Ignored when socket is set.";
    };

    logLevel = lib.mkOption {
      type = lib.types.enum ["trace" "debug" "info" "warn" "error"];
      default = "info";
      description = "Tracing log verbosity level.";
    };

    logFormat = lib.mkOption {
      type = lib.types.enum ["text" "json"];
      default = "json";
      description = ''
        Log output format.  Use "text" for human-readable local logs and
        "json" for structured logs consumed by a log aggregator.
      '';
    };

    dbPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/hyuqueue-server/hyuqueue.db";
      description = "Path to the SQLite database file.";
    };

    frontendPath = lib.mkOption {
      type = lib.types.str;
      default = "${cfg.package}/share/hyuqueue-server/frontend";
      defaultText =
        lib.literalExpression
        ''"''${cfg.package}/share/hyuqueue-server/frontend"'';
      description = "Path to compiled frontend static assets.";
    };

    baseUrl = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://example.com";
      description = ''
        Public base URL of the service, used to construct the OIDC redirect
        URI ("<baseUrl>/auth/callback").  Required when OIDC is enabled.
      '';
    };

    oidcIssuer = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://sso.example.com/application/o/hyuqueue";
      description = ''
        OIDC issuer URL used for provider discovery.  Set all three OIDC
        options or leave all three null for unauthenticated admin mode.
      '';
    };

    oidcClientId = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        OIDC client ID.  Set all three OIDC options or leave all three
        null for unauthenticated admin mode.
      '';
    };

    oidcClientSecretFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Path to a file containing the OIDC client secret.  Set all three
        OIDC options or leave all three null for unauthenticated admin
        mode.
      '';
    };

    llm = {
      baseUrl = lib.mkOption {
        type = lib.types.str;
        default = "http://localhost:11434/v1";
        description = "Base URL for the OpenAI-compatible LLM API.";
      };

      intakeModel = lib.mkOption {
        type = lib.types.str;
        default = "llama3.2";
        description = "Model name for intake LLM analysis.";
      };

      reviewModel = lib.mkOption {
        type = lib.types.str;
        default = "llama3.2";
        description = "Model name for review LLM analysis.";
      };
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "hyuqueue-server";
      description = "System user account the service runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "hyuqueue-server";
      description = "System group the service runs as.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = let
          oidcFields = [cfg.oidcIssuer cfg.oidcClientId cfg.oidcClientSecretFile];
          setCount = lib.count (x: x != null) oidcFields;
        in
          setCount == 0 || setCount == 3;
        message = ''
          services.hyuqueue-server: OIDC configuration is partial.
          Set all three of oidcIssuer, oidcClientId, and oidcClientSecretFile,
          or leave all three null for unauthenticated admin mode.
        '';
      }
    ];

    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      description = "hyuqueue-server service user";
    };

    users.groups.${cfg.group} = {};

    # Create the socket directory before the socket unit tries to bind.
    systemd.tmpfiles.rules = lib.mkIf (cfg.socket != null) [
      "d ${dirOf cfg.socket} 0750 ${cfg.user} ${cfg.group} -"
    ];

    # Socket unit: systemd creates and holds the Unix domain socket, then
    # passes the open file descriptor to the service on first activation.
    systemd.sockets.hyuqueue-server = lib.mkIf (cfg.socket != null) {
      description = "hyuqueue-server Unix domain socket";
      wantedBy = ["sockets.target"];
      socketConfig = {
        ListenStream = cfg.socket;
        SocketUser = cfg.user;
        SocketGroup = cfg.group;
        SocketMode = "0660";
        Accept = false;
      };
    };

    systemd.services.hyuqueue-server = {
      description = "hyuqueue-server service";
      wantedBy = ["multi-user.target"];
      after =
        ["network.target"]
        ++ lib.optional (cfg.socket != null) "hyuqueue-server.socket";
      requires =
        lib.optional (cfg.socket != null) "hyuqueue-server.socket";

      environment =
        {
          LOG_LEVEL = cfg.logLevel;
          LOG_FORMAT = cfg.logFormat;
          DB_PATH = cfg.dbPath;
        }
        // lib.optionalAttrs (cfg.baseUrl != null) {
          BASE_URL = cfg.baseUrl;
        }
        // lib.optionalAttrs (cfg.oidcIssuer != null) {
          OIDC_ISSUER = cfg.oidcIssuer;
        }
        // lib.optionalAttrs (cfg.oidcClientId != null) {
          OIDC_CLIENT_ID = cfg.oidcClientId;
        }
        // lib.optionalAttrs (cfg.oidcClientSecretFile != null) {
          OIDC_CLIENT_SECRET_FILE = cfg.oidcClientSecretFile;
        };

      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        WatchdogSec = lib.mkDefault "30s";

        ExecStart =
          "${cfg.package}/bin/hyuqueue-server"
          + (
            if cfg.socket != null
            then " --listen sd-listen"
            else " --listen ${cfg.host}:${toString cfg.port}"
          )
          + " --frontend-path ${cfg.frontendPath}";

        User = cfg.user;
        Group = cfg.group;
        Restart = "on-failure";
        RestartSec = "5s";
        StateDirectory = "hyuqueue-server";

        # Harden the service environment.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [
          (dirOf cfg.dbPath)
        ];
      };
    };
  };
}
