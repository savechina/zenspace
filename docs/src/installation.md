# Installation

## Prerequisites

- **macOS** (Apple Silicon or Intel) — primary supported platform
- **Rust toolchain** 1.80+ (only for building from source)

## Homebrew (macOS, Recommended)

```bash
brew install savechina/tap/zenspace
```

## From Source

```bash
git clone https://github.com/savechina/zenspace.git
cd zenspace
bin/build
./target/release/zen --help
```

## Binary Download

Download pre-built macOS binaries from [GitHub Releases](https://github.com/savechina/zenspace/releases):

- `zen-{version}-aarch64-apple-darwin.tar.gz` (Apple Silicon)
- `zen-{version}-x86_64-apple-darwin.tar.gz` (Intel)

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
