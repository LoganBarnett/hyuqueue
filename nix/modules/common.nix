# Shared seam for the hyuqueue-server NixOS and nix-darwin modules.
# Both platform entrypoints (`./nixos-server.nix` and
# `./darwin-server.nix`) merge this module in via their `imports`
# lists, so any declaration added here applies on both platforms.
#
# Hyuqueue-specific extensions over what `foundation.lib.mk*Service`
# provides live here:
#
# - Domain options (`dbPath`, `llm.*`, `topics`) that the generic
#   foundation helper doesn't know about.
# - The `config.toml` derivation the server reads via `--config`.
#   Topic configs are arbitrarily nested attrs that don't fit env
#   vars cleanly, so we ship the hyuqueue-only fields through one
#   file.
# - The `topicSubmodule` type — the open submodule that lets
#   topic-specific Nix modules render their typed options into
#   `services.hyuqueue-server.topics.<name>.{command,settings}`.
#
# The generated `config.toml` derivation is exposed via the internal
# `services.hyuqueue-server._generatedConfigFile` option so each
# platform module can wire it into the `<app>_config` env var
# without re-deriving the config attrset.
#
# *What's deliberately not in the generated `config.toml`:* every
# field the foundation already handles via env-var or CLI-flag
# injection — `log_level`, `log_format`, `base_url`, `oidc_*`,
# `listen`, `frontend_path`.  Foundation sets those at higher
# precedence than the config file, so duplicating them here would
# be dead weight that gets overwritten before the server reads it.
{
  config,
  lib,
  pkgs,
  ...
}: let
  name = "hyuqueue-server";
  cfg = config.services.${name};
  tomlFormat = pkgs.formats.toml {};

  # Filter to enabled topics and project each onto the TOML shape
  # the server's TopicConfigRaw expects: { id, command, config }.
  enabledTopics = lib.mapAttrsToList (topicName: topic: {
    id = topicName;
    inherit (topic) command;
    config = topic.settings;
  }) (lib.filterAttrs (_: t: t.enable) cfg.topics);

  # Only hyuqueue-owned fields land in the generated file —
  # everything the foundation handles is omitted (see header).
  configAttrs = {
    db_path = cfg.dbPath;
    llm = {
      base_url = cfg.llm.baseUrl;
      intake_model = cfg.llm.intakeModel;
      review_model = cfg.llm.reviewModel;
    };
    topics = enabledTopics;
  };

  configFile = tomlFormat.generate "${name}-config.toml" configAttrs;

  topicSubmodule = lib.types.submodule ({name, ...}: {
    options = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether this topic instance is included in the generated
          config.toml.  Defaults to true so that defining an
          instance implies enabling it; set to false to disable
          without removing the definition.
        '';
      };

      command = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        example =
          lib.literalExpression
          ''[ "''${pkgs.hyuqueue-topic-rss}/bin/hyuqueue-topic-rss" ]'';
        description = ''
          argv used to spawn the topic subprocess.  The first
          element must be the binary path; remaining elements are
          passed as arguments.  Topic-specific Nix modules
          typically set this to a derivation output path.
        '';
      };

      settings = lib.mkOption {
        type = tomlFormat.type;
        default = {};
        description = ''
          Topic-specific configuration, rendered as the
          `[topics.config]` table for this instance and passed
          verbatim to the topic subprocess on `init`.  Free-form;
          the topic owns the schema.

          Keys ending in `_cmd` are interpreted by the server at
          startup as shell commands whose stdout becomes the
          value under the same key without the suffix — use this
          for secret resolution (`api_key_cmd = "cat
          /run/secrets/x"`) to keep secrets out of the Nix store.
        '';
      };
    };

    config = {
      command = lib.mkDefault [];
    };
  });
in {
  options.services.${name} = {
    dbPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/${name}/hyuqueue.db";
      description = "Path to the SQLite database file.";
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

    topics = lib.mkOption {
      type = lib.types.attrsOf topicSubmodule;
      default = {};
      description = ''
        Topic instances to run.  Each attribute name becomes the
        `[[topics]].id` of one configured instance; the value
        declares how to spawn that instance and what to pass it.

        Open by design: the server module knows nothing about
        specific topics, so topic-specific Nix modules can render
        their own typed options into entries here without
        modifying the server module.
      '';
      example = lib.literalExpression ''
        {
          "rss-tech" = {
            command = [ "''${pkgs.hyuqueue-topic-rss}/bin/hyuqueue-topic-rss" ];
            settings.feeds = [ "https://example.com/feed" ];
          };
        }
      '';
    };

    # Internal: path to the generated config.toml.  Platform
    # modules reference this when they set the `<app>_config` env
    # var on systemd / launchd, so the server reads hyuqueue's
    # nested config (`llm`, `topics`, `db_path`) from one file.
    _generatedConfigFile = lib.mkOption {
      type = lib.types.path;
      internal = true;
      readOnly = true;
      description = ''
        Path to the generated config.toml.  Set by `common.nix`;
        consumed by the platform module's
        `<app>_config` env-var injection.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions =
      lib.mapAttrsToList (topicName: topic: {
        assertion = !topic.enable || topic.command != [];
        message = ''
          services.${name}.topics."${topicName}".command must be a
          non-empty argv when the topic is enabled.  Set it to the
          path of the topic subprocess binary (typically provided
          by the topic's own Nix module).
        '';
      })
      cfg.topics;

    services.${name}._generatedConfigFile = configFile;
  };
}
