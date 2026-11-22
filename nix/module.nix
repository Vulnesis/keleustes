# NixOS module: Keleustes headless, serving belai nodes without a window.
#
#   services.keleustes = {
#     enable = true;
#     listenAddress = "0.0.0.0:7443";
#     advertiseAddrs = [ "10.0.0.5:7443" ];
#     openFirewall = true;
#   };
#
# Join tokens are made on the host as the service user, against the same data
# directory:  sudo -u keleustes keleustes --data /var/lib/keleustes token new
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.keleustes;
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkOption
    mkIf
    types
    ;

  port = lib.toInt (lib.last (lib.splitString ":" cfg.listenAddress));

  # Settings live in the store, not in flags: `keleustes config KEY VALUE`
  # writes them before every start, so the module stays the source of truth.
  settings = {
    "node.listen_addr" = cfg.listenAddress;
  }
  // lib.optionalAttrs (cfg.advertiseAddrs != [ ]) {
    "node.advertise_addrs" = lib.concatStringsSep "," cfg.advertiseAddrs;
  }
  // cfg.settings;

  kls = "${lib.getExe cfg.package} --data ${cfg.dataDir}";
in
{
  options.services.keleustes = {
    enable = mkEnableOption "Keleustes headless (`keleustes --headless`), the server for a belai fleet";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.keleustes;
      defaultText = lib.literalExpression "keleustes.packages.\${system}.keleustes";
      description = "The keleustes package.";
    };

    user = mkOption {
      type = types.str;
      default = "keleustes";
      description = "User the service runs as. Created when left at the default.";
    };

    group = mkOption {
      type = types.str;
      default = "keleustes";
      description = "Group the service runs as. Created when left at the default.";
    };

    dataDir = mkOption {
      type = types.path;
      default = "/var/lib/keleustes";
      description = "Data directory: keleustes.db (SQLite), ca/, artifacts/ and vault.key.";
    };

    listenAddress = mkOption {
      type = types.str;
      default = "0.0.0.0:7443";
      example = "10.0.0.5:7443";
      description = "HOST:PORT nodes connect to (the `node.listen_addr` setting).";
    };

    advertiseAddrs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "factory.example.internal:7443" ];
      description = "Addresses join tokens tell nodes to dial (`node.advertise_addrs`). Empty: Keleustes picks from the host's interfaces.";
    };

    databaseUrl = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "postgres://keleustes@localhost/keleustes";
      description = "Use this database instead of <dataDir>/keleustes.db. For a secret in the URL use environmentFile with KELEUSTES_DATABASE_URL.";
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "systemd EnvironmentFile, e.g. KELEUSTES_DATABASE_URL=postgres://… and RUST_LOG.";
    };

    settings = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        "node.advertise_addrs" = "10.0.0.5:7443";
      };
      description = "Further `keleustes config KEY VALUE` settings applied before each start.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the listen port in the firewall.";
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra arguments to `keleustes --headless`.";
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    users.users = mkIf (cfg.user == "keleustes") {
      keleustes = {
        isSystemUser = true;
        group = cfg.group;
        home = cfg.dataDir;
        description = "Keleustes";
      };
    };
    users.groups = mkIf (cfg.group == "keleustes") { keleustes = { }; };

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ port ];

    systemd.tmpfiles.settings."10-keleustes".${cfg.dataDir}.d = {
      inherit (cfg) user group;
      mode = "0700";
    };

    systemd.services.keleustes = {
      description = "Keleustes (headless)";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ] ++ lib.optional (cfg.databaseUrl != null) "postgresql.service";

      environment = {
        RUST_LOG = lib.mkDefault "info";
        HOME = cfg.dataDir;
      }
      // lib.optionalAttrs (cfg.databaseUrl != null) { KELEUSTES_DATABASE_URL = cfg.databaseUrl; };

      preStart = lib.concatStringsSep "\n" (
        lib.mapAttrsToList (k: v: "${kls} config ${lib.escapeShellArg k} ${lib.escapeShellArg v}") settings
      );

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = "${kls} --headless ${lib.escapeShellArgs cfg.extraArgs}";
        Restart = "on-failure";
        RestartSec = 5;
        WorkingDirectory = cfg.dataDir;
        EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;
        UMask = "0077";

        # Hardening. Sessions and jobs run on the belai nodes, not here; the
        # local gh CLI (GitHub through this host) is the one thing it spawns.
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.dataDir ];
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        SystemCallArchitectures = "native";
        CapabilityBoundingSet = lib.optional (port < 1024) "CAP_NET_BIND_SERVICE";
        AmbientCapabilities = lib.optional (port < 1024) "CAP_NET_BIND_SERVICE";
      };
    };
  };
}
