{ self }:
{ config, lib, pkgs, ... }:

let
  inherit (lib) mkEnableOption mkIf mkOption types;

  cfg = config.services.linear-webhook;

  # systemd places LoadCredential= files here; the path is stable.
  credentialsDir = "/run/credentials/linear-webhook.service";

  absolutePath = types.strMatching "/.*";

  serverModule = { name, ... }: {
    options = {
      discordWebhookUrl = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Discord webhook URL (`https://discord.com/api/webhooks/<id>/<token>`).
          It ends up in the world-readable Nix store; prefer
          {option}`discordWebhookUrlFile`.
        '';
      };
      discordWebhookUrlFile = mkOption {
        type = types.nullOr absolutePath;
        default = null;
        example = "/run/agenix/linear-webhook-${name}-discord";
        description = ''
          File holding the Discord webhook URL. It is read via systemd
          `LoadCredential=`, so it can be owned by root with mode 0400.
        '';
      };
      linearSecret = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Linear webhook signing secret. It ends up in the world-readable Nix
          store; prefer {option}`linearSecretFile`.
        '';
      };
      linearSecretFile = mkOption {
        type = types.nullOr absolutePath;
        default = null;
        example = "/run/agenix/linear-webhook-${name}-secret";
        description = ''
          File holding the Linear webhook signing secret. It is read via
          systemd `LoadCredential=`, so it can be owned by root with mode 0400.
        '';
      };
    };
  };

  credentialName = server: what: "${server}-${what}";

  # One TOML table per server; `*File` options point into the credentials dir.
  serverSettings = name: s:
    lib.filterAttrs (_: v: v != null) {
      discord_webhook_url = s.discordWebhookUrl;
      discord_webhook_url_file =
        if s.discordWebhookUrlFile != null
        then "${credentialsDir}/${credentialName name "discord-webhook-url"}"
        else null;
      linear_secret = s.linearSecret;
      linear_secret_file =
        if s.linearSecretFile != null
        then "${credentialsDir}/${credentialName name "linear-secret"}"
        else null;
    };

  configFile = (pkgs.formats.toml { }).generate "linear-webhook.toml"
    (lib.mapAttrs serverSettings cfg.servers);

  credentials = lib.flatten (lib.mapAttrsToList
    (name: s:
      lib.optional (s.discordWebhookUrlFile != null)
        "${credentialName name "discord-webhook-url"}:${s.discordWebhookUrlFile}"
      ++ lib.optional (s.linearSecretFile != null)
        "${credentialName name "linear-secret"}:${s.linearSecretFile}")
    cfg.servers);

  serverAssertions = lib.flatten (lib.mapAttrsToList
    (name: s: [
      {
        assertion = builtins.match "[A-Za-z0-9_-]{1,64}" name != null;
        message = "services.linear-webhook.servers.\"${name}\": names must be 1-64 characters of [A-Za-z0-9_-].";
      }
      {
        assertion = (s.discordWebhookUrl == null) != (s.discordWebhookUrlFile == null);
        message = "services.linear-webhook.servers.${name}: set exactly one of discordWebhookUrl or discordWebhookUrlFile.";
      }
      {
        assertion = s.linearSecret == null || s.linearSecretFile == null;
        message = "services.linear-webhook.servers.${name}: linearSecret and linearSecretFile are mutually exclusive.";
      }
      {
        assertion = cfg.dangerouslyIgnoreSecret || s.linearSecret != null || s.linearSecretFile != null;
        message = "services.linear-webhook.servers.${name}: set linearSecret or linearSecretFile (or enable dangerouslyIgnoreSecret).";
      }
    ])
    cfg.servers);

  privilegedPort = cfg.listenPort < 1024;
in
{
  options.services.linear-webhook = {
    enable = mkEnableOption "the Linear to Discord webhook forwarder";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "linear-webhook.packages.\${system}.default";
      description = "The linear-webhook package to use.";
    };

    listenHost = mkOption {
      type = types.str;
      default = "127.0.0.1";
      example = "::";
      description = "IP address to listen on.";
    };

    listenPort = mkOption {
      type = types.port;
      default = 9999;
      description = "TCP port to listen on.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open {option}`listenPort` in the firewall.";
    };

    servers = mkOption {
      type = types.attrsOf (types.submodule serverModule);
      default = { };
      example = lib.literalExpression ''
        {
          myteam = {
            discordWebhookUrlFile = "/run/agenix/myteam-discord";
            linearSecretFile = "/run/agenix/myteam-linear";
          };
        }
      '';
      description = ''
        Servers to forward. Each one is served at `POST /<name>`; point the
        Linear webhook at `https://<host>/<name>`.
      '';
    };

    dangerouslyIgnoreSecret = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Accept deliveries without checking signatures or timestamps. Anyone
        who can reach the server can then post to your Discord channels.
      '';
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra command-line arguments for linear-webhook.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.servers != { };
        message = "services.linear-webhook.servers must define at least one server.";
      }
    ] ++ serverAssertions;

    warnings = lib.optional cfg.dangerouslyIgnoreSecret
      "services.linear-webhook.dangerouslyIgnoreSecret is enabled: webhook signatures are not checked.";

    networking.firewall.allowedTCPPorts = lib.optional cfg.openFirewall cfg.listenPort;

    systemd.services.linear-webhook = {
      description = "Linear to Discord webhook forwarder";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      restartTriggers = [ configFile ];

      serviceConfig = {
        ExecStart = lib.escapeShellArgs ([
          (lib.getExe cfg.package)
          "--config"
          configFile
          "--address"
          cfg.listenHost
          "--port"
          (toString cfg.listenPort)
        ]
        ++ lib.optional cfg.dangerouslyIgnoreSecret "--dangerously-ignore-secret"
        ++ cfg.extraArgs);
        LoadCredential = credentials;
        Restart = "on-failure";
        RestartSec = 5;

        DynamicUser = true;
        UMask = "0077";
        AmbientCapabilities = lib.optional privilegedPort "CAP_NET_BIND_SERVICE";
        CapabilityBoundingSet = lib.optional privilegedPort "CAP_NET_BIND_SERVICE";
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        PrivateUsers = !privilegedPort;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" "~@resources" ];
      };
    };
  };
}
