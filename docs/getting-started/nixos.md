---
title: NixOS
layout: default
parent: Getting Started
nav_order: 4
description: "Install Lumen on NixOS with the Lumen flake and NixOS module."
---

# NixOS

{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

Lumen ships a flake that builds the compositor and provides a NixOS module. The
module installs Lumen system-wide and exposes a `lumen@<username>` systemd
template service, mirroring the `.deb`/`.rpm` packaging.

The flake exposes:

| Output                        | Description                                              |
| ----------------------------- | -------------------------------------------------------- |
| `packages.<system>.lumen`     | The Lumen binary (and `default`)                         |
| `overlays.default`            | Adds `pkgs.lumen`                                        |
| `nixosModules.lumen`          | The NixOS module (also `default`)                        |
| `devShells.<system>.default`  | A `nix develop` build environment for contributors       |

---

## Install with the NixOS module

Add the flake as an input and import the module.

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    lumen = {
      url = "github:swedishborgie/lumen";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, lumen, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        lumen.nixosModules.lumen
        ({ ... }: {
          services.lumen = {
            enable = true;
            users = [ "alice" ];

            # Authentication and TLS
            auth = "basic";                       # none | basic | bearer | oauth2
            tlsCertPath = "/etc/lumen/cert.pem";
            tlsKeyPath = "/etc/lumen/key.pem";

            # Desktop to stream
            desktop = "labwc";                    # labwc | kde | none

            # Open the signalling, TURN and relay ports in the firewall
            openFirewall = true;
          };

          # The user must already exist.
          users.users.alice = {
            isNormalUser = true;
            home = "/home/alice";
          };
        })
      ];
    };
  };
}
```

Then rebuild:

```bash
sudo nixos-rebuild switch --flake .#myhost
```

---

## Configuration

Lumen is configured through environment variables. The module maps the common
ones to options and forwards everything else through `environment`.

### Options

| Option                                  | Default           | Description                                                  |
| --------------------------------------- | ----------------- | ------------------------------------------------------------ |
| `services.lumen.enable`                 | `false`           | Install and enable Lumen.                                    |
| `services.lumen.package`                | flake package     | Override the Lumen package.                                  |
| `services.lumen.users`                  | `[ ]`             | Existing users to run `lumen@<user>` instances for.          |
| `services.lumen.bindAddr`               | `0.0.0.0:8080`    | `LUMEN_BIND` — listen address and port.                      |
| `services.lumen.auth`                   | `none`            | `LUMEN_AUTH` — `none`, `basic`, `bearer` or `oauth2`.        |
| `services.lumen.desktop`                | `labwc`           | `LUMEN_DESKTOP` — `labwc`, `kde` or `none`.                  |
| `services.lumen.launch`                 | `null`            | `LUMEN_LAUNCH` — override the desktop launch command.        |
| `services.lumen.tlsCertPath`            | `null`            | `LUMEN_TLS_CERT` — PEM certificate chain.                    |
| `services.lumen.tlsKeyPath`             | `null`            | `LUMEN_TLS_KEY` — PEM private key.                           |
| `services.lumen.environment`            | `{ }`             | Extra (non-secret) `LUMEN_*` variables.                      |
| `services.lumen.environmentFile`        | `null`            | systemd `EnvironmentFile` for secrets.                       |
| `services.lumen.runtimeDir`             | `null`            | Explicit `XDG_RUNTIME_DIR` (single-user only).               |
| `services.lumen.extraGroups`            | `[ ]`             | Extra groups for every configured user.                      |
| `services.lumen.uinput.enable`          | `true`            | Enable `hardware.uinput` and add the `uinput` group.         |
| `services.lumen.openFirewall`           | `false`           | Open 8080/TCP, 3478/UDP and 50000–50010/UDP.                 |
| `services.lumen.restart`                | `always`          | systemd `Restart=` policy. `always` (not `on-failure`) because Lumen exits 0 when the nested desktop exits. |
| `services.lumen.extraAfter`             | `[ ]`             | Additional units to order instances after.                   |
| `services.lumen.extraWants`             | `[ ]`             | Additional units instances should softly depend on.          |

The package is built from the flake's pinned `nixpkgs`, so the binary matches
the rest of the Lumen release rather than your system channel.

### Secrets

Never put secrets in `services.lumen.environment`: the Nix store is
world-readable. Put them in an `environmentFile` instead:

```nix
services.lumen.environmentFile = "/run/secrets/lumen.env";
```

That file is a plain systemd environment file:

```bash
LUMEN_AUTH_BEARER_TOKEN=...
LUMEN_TURN_PASSWORD=...
LUMEN_AUTH_OAUTH2_CLIENT_SECRET=...
```

With [sops-nix](https://github.com/Mic92/sops-nix), point it at a decrypted
secret:

```nix
sops.secrets."lumen/env" = {
  sopsFile = ./secrets/lumen.env;
  format = "dotenv";
  owner = "alice";
};

services.lumen.environmentFile = config.sops.secrets."lumen/env".path;
```

### PAM authentication

When `auth = "basic"`, the module creates a `lumen` PAM service and sets
`LUMEN_AUTH_PAM_SERVICE=lumen`, so Lumen does not depend on the `sshd` PAM
service being present. Credentials are validated against the system password
database, and the submitted username must match the user running the instance.

---

## Running the service

The module installs a systemd [template unit](https://www.freedesktop.org/software/systemd/man/systemd.service.html#Service%20Templates).
The instance name is the username:

```bash
# Status
systemctl status lumen@alice

# Start / stop
sudo systemctl start lumen@alice
sudo systemctl stop lumen@alice

# Logs
journalctl -u lumen@alice -f
```

Instances for the users listed in `services.lumen.users` are enabled
automatically at boot.

Once running, open a browser and navigate to `http://<host>:8080`.

---

## Install without the module

You can also install just the package:

```bash
# Ad-hoc
nix profile install github:swedishborgie/lumen

# System-wide, via your NixOS configuration
environment.systemPackages = [ lumen.packages.x86_64-linux.lumen ];
```

Or use the overlay:

```nix
nixpkgs.overlays = [ lumen.overlays.default ];
environment.systemPackages = [ pkgs.lumen ];
```

---

## Development

The flake provides a build shell with the full Rust and native dependency set,
so no per-distribution package installation is required:

```bash
nix develop
cargo build --release
```
