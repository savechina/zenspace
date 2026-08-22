# Installation

## Prerequisites

### macOS

- **macOS** (Apple Silicon or Intel) — primary supported platform
- **Rust toolchain** 1.80+ (only for building from source)
- No additional dependencies — `sandbox-exec` is built-in

### Linux

- **Rust toolchain** 1.80+ (only for building from source)
- **bubblewrap** (`bwrap`) — required for sandbox isolation

```bash
# Ubuntu/Debian
sudo apt install bubblewrap

# Fedora
sudo dnf install bubblewrap

# Arch
sudo pacman -S bubblewrap
```

**Why bubblewrap?** Zen uses Linux namespaces for sandbox isolation. Bubblewrap provides unprivileged sandboxing without requiring root access.

## Homebrew (macOS & Linux, Recommended)

```bash
brew install savechina/tap/zenspace
```

**Note:** On Linux, Homebrew (Linuxbrew) also handles bubblewrap dependency automatically.

## From Source

```bash
git clone https://github.com/savechina/zenspace.git
cd zenspace
bin/build
./target/release/zen --help
```

## Binary Download

Download pre-built binaries from [GitHub Releases](https://github.com/savechina/zenspace/releases):

**macOS:**
- `zen-{version}-aarch64-apple-darwin.tar.gz` (Apple Silicon)
- `zen-{version}-x86_64-apple-darwin.tar.gz` (Intel)

**Linux:**
- `zen-{version}-x86_64-unknown-linux-gnu.tar.gz` (x86_64)
- `zen-{version}-aarch64-unknown-linux-gnu.tar.gz` (ARM64)

**Linux Note:** After downloading, install bubblewrap:
```bash
sudo apt install bubblewrap  # Ubuntu/Debian
```

## Post-Installation

Verify the installation:

```bash
zen version
```

Initialize your workspace:

```bash
zen workspace init
```

This creates the `~/.zen/` directory structure and default configuration.

---

Next: [Quick Start](quickstart.md)
