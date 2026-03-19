# Lumen — Package Build & Installation

This directory contains everything needed to build and install Lumen as a native system package (`.deb` for Ubuntu/Debian, `.rpm` for Fedora/RHEL).

---

## Building the packages

Packages are built inside Docker so you don't need any build dependencies on your host. The only requirement is Docker (or Podman).

```bash
# From the repository root
docker build -f docker/Dockerfile.packages -t lumen-packages .

# With an explicit version
docker build --build-arg VERSION=1.2.3 -f docker/Dockerfile.packages -t lumen-packages .

# .deb only (skip the Fedora/RPM build)
docker build --build-arg BUILD_RPM=0 -f docker/Dockerfile.packages -t lumen-packages .

# .rpm only (skip the Ubuntu/deb build)
docker build --build-arg BUILD_DEB=0 -f docker/Dockerfile.packages -t lumen-packages .
```

> The first build takes a while — it compiles Rust, all native C/C++ dependencies, and the packaging tools from scratch for both Ubuntu and Fedora. Subsequent builds reuse Docker's layer cache as long as `Cargo.toml`, `Cargo.lock`, and system dependencies haven't changed.

### Extracting the packages

Mount a local directory to `/output` and run the image. Both packages are copied out automatically:

```bash
mkdir -p dist
docker run --rm -v ./dist:/output lumen-packages
```

You will find the packages in `./dist/`:

```
dist/
├── lumen_0.1.0_amd64.deb
└── lumen-0.1.0-1.x86_64.rpm
```

---

## Installation

### Ubuntu / Debian (.deb)

```bash
sudo apt install ./dist/lumen_*.deb
```

`apt` handles all runtime dependencies automatically. After installation, the post-install script will:

1. Create `/etc/lumen/` (mode `750`)
2. Copy `example.env` to `/etc/lumen/example.env`
3. Print a getting-started message

### Fedora / RHEL / CentOS (.rpm)

```bash
sudo dnf install ./dist/lumen-*.rpm
```

The same setup steps run as a post-install scriptlet.

---

## Configuration

### 1. Create a config file for the user you want to run Lumen as

```bash
sudo cp /etc/lumen/example.env /etc/lumen/<username>.env
sudo nano /etc/lumen/<username>.env
```

`/etc/lumen/<username>.env` is admin-managed and loaded by the systemd service. At minimum, set `LUMEN_LAUNCH` to the Wayland compositor or desktop you want to stream:

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

| File | Managed by | Priority |
|------|-----------|----------|
| `/etc/lumen/<username>.env` | Admin | Lower |
| `~/.config/lumen/env` | User | Higher (wins) |

Both files are optional — the service starts without them using built-in defaults.

### Authentication

Set `LUMEN_AUTH` in your env file to control who can access the stream:

| Mode | Description |
|------|-------------|
| `none` | No authentication (default) |
| `basic` | HTTP Basic auth validated against system PAM |
| `bearer` | Preshared bearer token (`LUMEN_AUTH_BEARER_TOKEN`) |
| `oauth2` | OpenID Connect / OIDC (`LUMEN_AUTH_OAUTH2_*` variables) |

See [`example.env`](example.env) for all required variables for each mode.

---

## Running the service

Lumen is installed as a [systemd template service](https://www.freedesktop.org/software/systemd/man/systemd.service.html#Service%20Templates). The instance name is the username to run as.

### Start

```bash
sudo systemctl start lumen@<username>
```

### Stop

```bash
sudo systemctl stop lumen@<username>
```

### Enable autostart on boot (optional)

The service is **not** enabled automatically at install time. Enable it explicitly when you want it:

```bash
sudo systemctl enable lumen@<username>
```

### View logs

```bash
journalctl -u lumen@<username> -f
```

### Example: full setup for user `alice`

```bash
# 1. Create config
sudo cp /etc/lumen/example.env /etc/lumen/alice.env
sudo nano /etc/lumen/alice.env   # set LUMEN_LAUNCH, auth, etc.

# 2. Start
sudo systemctl start lumen@alice

# 3. Open a browser to http://<host>:8080

# 4. (Optional) enable autostart
sudo systemctl enable lumen@alice
```

---

## How the service works

The template service (`lumen@.service`) runs under `User=<username>` with a full PAM login session — similar to how a display manager starts a desktop session. This means:

- `XDG_RUNTIME_DIR` is provisioned at `/run/user/<uid>` by systemd-logind
- The session appears in `loginctl list-sessions`
- PAM modules (e.g. limits, environment) apply normally
- GPU and audio devices are accessible via the user's group memberships

If `LUMEN_LAUNCH` is set, Lumen starts the specified Wayland client (e.g. `labwc`) and shuts down gracefully when that client exits. If `LUMEN_LAUNCH` is not set, Lumen runs until stopped manually or by the service manager.

---

## Uninstalling

```bash
# Debian/Ubuntu
sudo apt remove lumen

# Fedora/RHEL
sudo dnf remove lumen
```

Configuration files in `/etc/lumen/` are not removed automatically so your settings are preserved across reinstalls. Remove them manually if needed:

```bash
sudo rm -rf /etc/lumen/
```
