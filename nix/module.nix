# Lumen — NixOS module.
#
# System-wide install of the `lumen` package plus a `lumen@<user>` systemd
# template service (mirroring the .deb/.rpm packaging). Personalisation and
# secrets stay out of this module: feed them through `environment` and
# `environmentFile` (typically a sops-nix secret).
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.lumen;
  inherit (lib)
    mkIf
    mkOption
    mkEnableOption
    types
    literalExpression
    optional
    optionalAttrs
    ;

  # Runtime packages for the selected desktop preset. `/run/current-system/sw`
  # is also on PATH, so system packages (and the KDE stack) are reachable.
  desktopPath =
    if cfg.desktop == "labwc" then
      [ pkgs.labwc pkgs.foot ]
    else if cfg.desktop == "kde" then
      [ pkgs.kdePackages.plasma-workspace ]
    else
      [ ];

  # NixOS-specific Qt/QML/gdk-pixbuf environment required by a Plasma session
  # started from a systemd system service. These paths are generic NixOS
  # layout, not host-specific.
  desktopEnv = optionalAttrs (cfg.desktop == "kde") {
    XDG_DATA_DIRS = "/run/current-system/sw/share";
    QT_PLUGIN_PATH = "/run/current-system/sw/lib/qt-6/plugins";
    # startplasma-wayland's wrapper prefixes per-package store paths onto this
    # variable, so it must exist to be a valid path list.
    NIXPKGS_QT6_QML_IMPORT_PATH = "/run/current-system/sw/lib/qt-6/qml";
    GI_TYPELIB_PATH = "/run/current-system/sw/lib/girepository-1.0";
    GDK_PIXBUF_MODULE_FILE = "${pkgs.librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache";
    KDE_APPLICATIONS_AS_SCOPE = "1";
    QT_WAYLAND_RECONNECT = "1";
    XDG_SESSION_CLASS = "user";
    XDG_SESSION_TYPE = "wayland";
  };

  # A normal user's UID is only assigned at activation, so it cannot be read
  # from `config.users.users.<name>.uid` (it is null at eval time). Resolve
  # runtime paths at start-up instead, where the process already runs as the
  # service user. `PAMName=login` creates `/run/user/<uid>` via pam_systemd.
  lumenLauncher = pkgs.writeShellScript "lumen-launch" ''
    ${lib.optionalString (cfg.runtimeDir == null) ''
      export XDG_RUNTIME_DIR=/run/user/$(${pkgs.coreutils}/bin/id -u)
    ''}
    ${lib.optionalString (cfg.desktop == "kde") ''
      export KDE_SESSION_UID=$(${pkgs.coreutils}/bin/id -u)
    ''}
    exec ${cfg.package}/bin/lumen
  '';
in
{
  options.services.lumen = {
    enable = mkEnableOption "Lumen, a Wayland WebRTC streaming compositor";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The Lumen package to install and run.";
    };

    users = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Existing system users to run `lumen@<user>` instances for. Each user
        is added to the groups needed for GPU, audio and (optionally) uinput
        access, and a `lumen@<user>.service` instance is enabled.
      '';
    };

    bindAddr = mkOption {
      type = types.str;
      default = "0.0.0.0:8080";
      description = "Address and port the Lumen HTTP/WebSocket server listens on (`LUMEN_BIND`).";
    };

    auth = mkOption {
      type = types.enum [
        "none"
        "basic"
        "bearer"
        "oauth2"
      ];
      default = "none";
      description = ''
        Authentication mode (`LUMEN_AUTH`). `basic` validates credentials
        against the `lumen` PAM service created by this module; the other
        modes read their credentials from {option}`services.lumen.environmentFile`.
      '';
    };

    desktop = mkOption {
      type = types.enum [
        "labwc"
        "kde"
        "none"
      ];
      default = "labwc";
      description = ''
        Desktop preset launched inside the compositor (`LUMEN_DESKTOP`).
        `labwc` pulls in labwc + foot, `kde` pulls in KDE Plasma and the
        NixOS Qt environment, `none` launches nothing.
      '';
    };

    launch = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "sway";
      description = "Override the desktop preset's launch command (`LUMEN_LAUNCH`).";
    };

    tlsCertPath = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "PEM certificate chain for HTTPS/WSS (`LUMEN_TLS_CERT`).";
    };

    tlsKeyPath = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "PEM private key for HTTPS/WSS (`LUMEN_TLS_KEY`).";
    };

    runtimeDir = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/user/1000";
      description = ''
        Explicit `XDG_RUNTIME_DIR` (and tmpfiles entry) for a **single-user**
        deployment, matching the packaged unit's behaviour on headless hosts.
        Leave null to derive `/run/user/$(id -u)` at start-up and rely on
        `PAMName=login`/pam_systemd to provision the directory.
      '';
    };

    environment = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = { LUMEN_VIDEO_BITRATE_KBPS = "8000"; };
      description = ''
        Extra Lumen environment variables applied to every instance. Do
        **not** put secrets here: the Nix store is world-readable. Use
        {option}`services.lumen.environmentFile` for those.
      '';
    };

    environmentFile = mkOption {
      type = types.nullOr (types.either types.path types.str);
      default = null;
      example = "/run/secrets/lumen.env";
      description = ''
        Optional systemd `EnvironmentFile` loaded by every instance, for
        secrets such as `LUMEN_AUTH_BEARER_TOKEN`,
        `LUMEN_AUTH_OAUTH2_CLIENT_SECRET` or `LUMEN_TURN_PASSWORD`. On NixOS
        this typically points at a sops-nix secret path.
      '';
    };

    extraGroups = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Additional groups to add to every configured user.";
    };

    uinput = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Enable `hardware.uinput` and add each user to the `uinput` group so
          the virtual gamepad support can access `/dev/uinput`.
        '';
      };
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the Lumen signalling, TURN and relay ports in the firewall.";
    };

    extraAfter = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Additional systemd units to order the Lumen instances after.";
    };

    extraWants = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Additional systemd units the Lumen instances softly depend on.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.users != [ ];
        message = "services.lumen: at least one user must be listed in services.lumen.users.";
      }
      {
        assertion = cfg.runtimeDir == null || builtins.length cfg.users == 1;
        message = "services.lumen: runtimeDir is single-user only; list exactly one user or leave it null.";
      }
    ];

    # ---- System-wide installation --------------------------------------------

    environment.systemPackages = [ cfg.package ];

    # GPU / audio stack the compositor expects.
    hardware.graphics.enable = lib.mkDefault true;
    hardware.uinput.enable = lib.mkDefault cfg.uinput.enable;
    services.pipewire.enable = lib.mkDefault true;
    security.rtkit.enable = lib.mkDefault true;

    users.users = lib.genAttrs cfg.users (user: {
      extraGroups =
        [
          "video"
          "render"
        ]
        ++ optional cfg.uinput.enable "uinput"
        ++ cfg.extraGroups;
    });

    # Lumen defaults to the `sshd` PAM service for HTTP Basic auth, which only
    # exists when OpenSSH is enabled. Point it at a dedicated service instead.
    security.pam.services.lumen = mkIf (cfg.auth == "basic") {
      unixAuth = true;
    };

    # ---- systemd template + per-user instances --------------------------------

    systemd.services =
      {
        "lumen@" = {
          description = "Lumen Wayland WebRTC compositor (%i)";

          after = [ "network.target" ] ++ cfg.extraAfter;
          wants = cfg.extraWants;

          path = [ pkgs.dbus cfg.package "/run/current-system/sw" ] ++ desktopPath;

          environment = {
            LUMEN_BIND = cfg.bindAddr;
            LUMEN_AUTH = cfg.auth;
            LUMEN_LOG_OUTPUT = "journald";
            LUMEN_SYSLOG_IDENTIFIER = "lumen@%i";
          }
          // optionalAttrs (cfg.desktop != "none") { LUMEN_DESKTOP = cfg.desktop; }
          // optionalAttrs (cfg.launch != null) { LUMEN_LAUNCH = cfg.launch; }
          // optionalAttrs (cfg.tlsCertPath != null) { LUMEN_TLS_CERT = cfg.tlsCertPath; }
          // optionalAttrs (cfg.tlsKeyPath != null) { LUMEN_TLS_KEY = cfg.tlsKeyPath; }
          // optionalAttrs (cfg.auth == "basic") { LUMEN_AUTH_PAM_SERVICE = "lumen"; }
          // optionalAttrs (cfg.runtimeDir != null) { XDG_RUNTIME_DIR = cfg.runtimeDir; }
          // desktopEnv
          // cfg.environment;

          serviceConfig = {
            Type = "simple";
            User = "%i";
            # Full PAM login session, matching the packaged unit: provisions
            # XDG_RUNTIME_DIR via systemd-logind and applies PAM limits.
            PAMName = "login";
            ExecStart = "${lumenLauncher}";
            Restart = "on-failure";
            RestartSec = "5s";
            StandardOutput = "journal";
            StandardError = "journal";
            EnvironmentFile = [ "-/etc/lumen/%i.env" ]
              ++ optional (cfg.environmentFile != null) cfg.environmentFile;
          };
        };
      }
      # Each declared user gets an enabled instance. `asDropin` makes
      # `lumen@<user>.service` extend the template above instead of shadowing
      # it with an empty unit file.
      // lib.genAttrs (map (user: "lumen@${user}") cfg.users) (
        unitName:
        let
          user = lib.removePrefix "lumen@" unitName;
          u = config.users.users.${user};
        in
        {
          enable = true;
          wantedBy = [ "multi-user.target" ];
          overrideStrategy = "asDropin";

          environment = {
            HOME = u.home;
          };

          serviceConfig.WorkingDirectory = u.home;
        }
      );

    # When an explicit runtime dir is requested, create it with the right
    # ownership. Otherwise pam_systemd provisions `/run/user/<uid>` for the
    # `PAMName=login` session at start-up.
    systemd.tmpfiles.rules = lib.optionals (cfg.runtimeDir != null) (
      map (user: "d ${cfg.runtimeDir} 0700 ${user} - - -") cfg.users
    );

    # ---- Networking -----------------------------------------------------------

    networking.firewall.allowedTCPPorts = optional cfg.openFirewall 8080;
    networking.firewall.allowedUDPPorts = optional cfg.openFirewall 3478;
    networking.firewall.allowedUDPPortRanges = optional cfg.openFirewall {
      from = 50000;
      to = 50010;
    };
  };
}
