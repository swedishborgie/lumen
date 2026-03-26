---
title: Fedora / RHEL
layout: default
parent: Getting Started
nav_order: 3
description: "Install Lumen on Fedora or RHEL using a native .rpm package with systemd service integration."
---

# Fedora / RHEL

{: .no_toc }

<details open markdown="block">
  <summary>On this page</summary>
  {: .text-delta }
- TOC
{:toc}
</details>

Install Lumen on Fedora, RHEL, or CentOS using a native `.rpm` package. The package includes a systemd template service so you can run Lumen as any user on the system.

---

## Download the Package

Download the latest `.rpm` from the [Lumen releases page](https://github.com/swedishborgie/lumen/releases/latest).

---

## Install

```bash
sudo dnf install ./lumen-*.rpm
```

`dnf` installs all runtime dependencies automatically. After installation, the post-install scriptlet creates `/etc/lumen/` and copies in a sample configuration file.

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
# 1. Download the package from https://github.com/swedishborgie/lumen/releases/latest
# 2. Install it
sudo dnf install ./lumen-*.rpm

# 3. Create config
sudo cp /etc/lumen/example.env /etc/lumen/alice.env
sudo nano /etc/lumen/alice.env   # set LUMEN_LAUNCH, auth, etc.

# 4. Start the service
sudo systemctl start lumen@alice

# 5. Open a browser to http://<host>:8080

# 6. (Optional) enable autostart
sudo systemctl enable lumen@alice
```

---

## Uninstall

```bash
sudo dnf remove lumen
```

Configuration files in `/etc/lumen/` are preserved across uninstall. Remove them manually if needed:

```bash
sudo rm -rf /etc/lumen/
```
