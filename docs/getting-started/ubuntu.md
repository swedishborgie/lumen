---
title: Ubuntu / Debian
layout: default
parent: Getting Started
nav_order: 2
description: "Install Lumen on Ubuntu or Debian using a native .deb package with systemd service integration."
---

# Ubuntu / Debian

{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

Install Lumen on Ubuntu or Debian using a native `.deb` package. The package includes a systemd template service so you can run Lumen as any user on the system.

---

## Prerequisites

You need **Podman** (or Docker) on any Linux machine to build the packages — no Rust or build dependencies are required on the target host.

---

## Build the Package

From the repository root:

```bash
# Build the package image
podman build -f docker/Dockerfile.packages -t lumen-packages .

# Extract the .deb (and .rpm) into ./dist/
mkdir -p dist
podman run --rm -v ./dist:/output lumen-packages
```

The resulting package will be at `./dist/lumen_<version>_amd64.deb`.

{: .tip }
To build only the `.deb` (skip the Fedora/RPM build):

```bash
podman build --build-arg BUILD_RPM=0 -f docker/Dockerfile.packages -t lumen-packages .
```

---

## Install

```bash
sudo apt install ./dist/lumen_*.deb
```

`apt` installs all runtime dependencies automatically. After installation, the post-install script creates `/etc/lumen/` and copies in a sample configuration file.

---

## Configure

### 1. Create a config file

Create a configuration file named after the user you want Lumen to run as:

```bash
sudo cp /etc/lumen/example.env /etc/lumen/<username>.env
sudo nano /etc/lumen/<username>.env
```

At minimum, set `LUMEN_LAUNCH` to the Wayland compositor or application you want to stream:

```bash
LUMEN_LAUNCH=labwc
```

See `example.env` for the full list of options with explanations.

### 2. User-managed overrides (optional)

Users can place their own overrides in `~/.config/lumen/env`. This file is loaded **after** `/etc/lumen/<username>.env`, so values here take precedence:

```bash
mkdir -p ~/.config/lumen
nano ~/.config/lumen/env
```

### Config file precedence

| File                        | Managed by | Priority      |
| --------------------------- | ---------- | ------------- |
| `/etc/lumen/<username>.env` | Admin      | Lower         |
| `~/.config/lumen/env`       | User       | Higher (wins) |

### Authentication

Set `LUMEN_AUTH` to control who can access the stream:

| Mode     | Description                                             |
| -------- | ------------------------------------------------------- |
| `none`   | No authentication (default)                             |
| `basic`  | HTTP Basic auth validated against system PAM            |
| `bearer` | Preshared bearer token (`LUMEN_AUTH_BEARER_TOKEN`)      |
| `oauth2` | OpenID Connect / OIDC (`LUMEN_AUTH_OAUTH2_*` variables) |

---

## Start the Service

Lumen is installed as a [systemd template service](https://www.freedesktop.org/software/systemd/man/systemd.service.html#Service%20Templates). The instance name is the username to run the session as.

```bash
# Start
sudo systemctl start lumen@<username>

# Stop
sudo systemctl stop lumen@<username>

# Enable autostart on boot (optional)
sudo systemctl enable lumen@<username>
```

Once running, open a browser and navigate to `http://<host>:8080`.

{: .note }
The service runs under `User=<username>` with a full PAM login session — similar to how a display manager starts a desktop session. `XDG_RUNTIME_DIR` is provisioned by systemd-logind, GPU and audio devices are accessible via group memberships, and PAM modules apply normally.

---

## View Logs

```bash
journalctl -u lumen@<username> -f
```

---

## Full Example: Setting Up for User `alice`

```bash
# 1. Build and install the package
podman build -f docker/Dockerfile.packages -t lumen-packages .
mkdir -p dist && podman run --rm -v ./dist:/output lumen-packages
sudo apt install ./dist/lumen_*.deb

# 2. Create config
sudo cp /etc/lumen/example.env /etc/lumen/alice.env
sudo nano /etc/lumen/alice.env   # set LUMEN_LAUNCH, auth, etc.

# 3. Start the service
sudo systemctl start lumen@alice

# 4. Open a browser to http://<host>:8080

# 5. (Optional) enable autostart
sudo systemctl enable lumen@alice
```

---

## Uninstall

```bash
sudo apt remove lumen
```

Configuration files in `/etc/lumen/` are preserved across uninstall. Remove them manually if needed:

```bash
sudo rm -rf /etc/lumen/
```
