#!/usr/bin/env bash
set -eu

# Morpheus CLI Install Script
#
# Downloads the latest Morpheus CLI binary from GitHub releases and installs it.
#
# Supported OS: Linux, macOS (darwin), Windows (MSYS2/Git Bash/WSL)
# Supported Architectures: x86_64 (x64), arm64/aarch64 (arm64)
#   Note: Windows only supports x64
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/aidreamy/morpheus/main/download.sh | bash
#
# Environment variables:
#   MORPHEUS_BIN_DIR  - Installation directory (default: $HOME/.local/bin, Windows: %USERPROFILE%/morpheus)
#   MORPHEUS_VERSION  - Specific version to install (e.g., "v0.1.0" or "0.1.0")
#   MORPHEUS_VARIANT  - Package variant: "slim" (default, no RAG, no Home),
#                   "slim-embed" (local RAG via fastembed, no Home) or
#                   "slim-speak" (voice output with the complete language
#                   set, PT-BR included; Linux and macOS only)
#   INSTALL_OS    - Override OS detection: "linux", "darwin", or "win32"
#
# Release artifacts are produced by .github/workflows/release.yml on tag push
# (v*) and published at github.com/aidreamy/morpheus/releases.

REPO="aidreamy/morpheus"
OUT_FILE="morpheus"

if [[ "${WINDIR:-}" ]] || [[ "${windir:-}" ]] || [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
    DEFAULT_BIN_DIR="$USERPROFILE/morpheus"
else
    DEFAULT_BIN_DIR="$HOME/.local/bin"
fi

MORPHEUS_BIN_DIR="${MORPHEUS_BIN_DIR:-$DEFAULT_BIN_DIR}"
MORPHEUS_VARIANT="${MORPHEUS_VARIANT:-slim}"

if ! command -v curl >/dev/null 2>&1; then
  echo "Error: 'curl' is required to download morpheus. Please install curl and try again."
  exit 1
fi

if [ -n "${MORPHEUS_VERSION:-}" ]; then
  if [[ ! "$MORPHEUS_VERSION" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-.*)?$ ]]; then
    echo "Error: invalid version '$MORPHEUS_VERSION'. Expected semver format (e.g. 1.0.0 or v1.0.0)"
    exit 1
  fi
  MORPHEUS_VERSION="v${MORPHEUS_VERSION#v}"
else
  echo "Fetching latest version..."
  LATEST_TAG=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
  if [ -z "$LATEST_TAG" ]; then
    echo "Error: Could not determine latest version. Set MORPHEUS_VERSION to specify one."
    exit 1
  fi
  MORPHEUS_VERSION="$LATEST_TAG"
fi

# Detect OS
if [ -n "${INSTALL_OS:-}" ]; then
  case "${INSTALL_OS}" in
    linux|darwin|win32) OS="${INSTALL_OS}" ;;
    *) echo "Error: unsupported INSTALL_OS='${INSTALL_OS}' (expected: linux|darwin|win32)"; exit 1 ;;
  esac
else
  if [[ "${WINDIR:-}" ]] || [[ "${windir:-}" ]] || [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
    OS="win32"
  elif [[ -f "/proc/version" ]] && grep -q "Microsoft\|WSL" /proc/version 2>/dev/null; then
    OS="linux"
  elif [[ "$OSTYPE" == "darwin"* ]]; then
    OS="darwin"
  elif [[ "$PWD" =~ ^/[a-zA-Z]/ ]] && { [[ -d "/c" ]] || [[ -d "/d" ]] || [[ -d "/e" ]]; }; then
    OS="win32"
  else
    case "$(uname -s | tr '[:upper:]' '[:lower:]')" in
      linux) OS="linux" ;;
      darwin) OS="darwin" ;;
      mingw*|msys*|cygwin*) OS="win32" ;;
      *) echo "Error: Unsupported OS. morpheus supports Linux, macOS, and Windows."; exit 1 ;;
    esac
  fi
fi

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  x86_64) ARCH="x64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *) echo "Error: Unsupported architecture '$ARCH'."; exit 1 ;;
esac

if [ "$OS" = "win32" ] && [ "$ARCH" != "x64" ]; then
  echo "Error: Windows currently only supports x64 architecture."
  exit 1
fi

# Variant note: every variant is built with --no-default-features.
# "slim-embed" additionally enables the workspace `embed` feature
# (morpheus-tools/embed + fastembed) for local RAG. slim-embed is
# produced for linux-x64 and win32-x64; macOS (both x64 and arm64) ships slim
# only - ort-sys has no prebuilt ONNX Runtime for x86_64-apple-darwin, and
# the aarch64-apple-darwin distribution (CoreML build) fails to static-link
# on the CI runner. "slim-speak" enables `speak-phonemize` (the complete
# voice build, which is what makes PT-BR work) and is produced for linux-x64
# and macOS only: the voice daemon uses a Unix domain socket and Unix process
# APIs, so there is no Windows build.
case "$MORPHEUS_VARIANT" in
  slim|slim-embed|slim-speak) ;;
  *) echo "Error: Unsupported MORPHEUS_VARIANT '$MORPHEUS_VARIANT'. Expected 'slim', 'slim-embed' or 'slim-speak'."; exit 1 ;;
esac

if [ "$MORPHEUS_VARIANT" = "slim-speak" ] && [ "$OS" = "win32" ]; then
  echo "Error: the 'slim-speak' variant (voice output) is not supported on Windows yet."
  echo "Use MORPHEUS_VARIANT=slim or MORPHEUS_VARIANT=slim-embed instead."
  exit 1
fi

SUFFIX="-$MORPHEUS_VARIANT"

VERSION="${MORPHEUS_VERSION#v}"
if [ "$OS" = "win32" ]; then
  FILE="morpheus-v${VERSION}-${OS}-${ARCH}${SUFFIX}.zip"
  EXTRACT_CMD="unzip"
  BINARY="morpheus.exe"
else
  FILE="morpheus-v${VERSION}-${OS}-${ARCH}${SUFFIX}.tar.gz"
  EXTRACT_CMD="tar"
  BINARY="morpheus"
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$MORPHEUS_VERSION/$FILE"

echo "Downloading morpheus $MORPHEUS_VERSION ($MORPHEUS_VARIANT variant) for $OS-$ARCH..."
if ! curl -sLf "$DOWNLOAD_URL" --output "$FILE"; then
  echo "Error: Failed to download $DOWNLOAD_URL"
  echo ""
  echo "If you requested a variant, make sure it exists for this release."
  echo "Try setting MORPHEUS_VARIANT=slim or removing the environment variable."
  exit 1
fi

TMP_DIR=$(mktemp -d "/tmp/morpheus_install_XXXXXX")
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Extracting..."
if [ "$EXTRACT_CMD" = "tar" ]; then
  tar xzf "$FILE" -C "$TMP_DIR"
else
  unzip -q "$FILE" -d "$TMP_DIR"
fi
rm "$FILE"

EXTRACT_DIR="$TMP_DIR"
if [ "$OS" = "win32" ] && [ -d "$TMP_DIR/morpheus-package" ]; then
  EXTRACT_DIR="$TMP_DIR/morpheus-package"
fi

if [ ! -f "$EXTRACT_DIR/$BINARY" ]; then
  echo "Error: $BINARY not found in extracted archive"
  exit 1
fi

chmod +x "$EXTRACT_DIR/$BINARY" "$EXTRACT_DIR/morpheus-uninstall" "$EXTRACT_DIR/morpheus-ttsd" 2>/dev/null || true

if [ ! -d "$MORPHEUS_BIN_DIR" ]; then
  mkdir -p "$MORPHEUS_BIN_DIR"
fi

echo "Installing morpheus to $MORPHEUS_BIN_DIR/"
if [ -f "$MORPHEUS_BIN_DIR/$OUT_FILE" ]; then
  mv "$MORPHEUS_BIN_DIR/$OUT_FILE" "$MORPHEUS_BIN_DIR/$OUT_FILE.old"
  if ! mv "$EXTRACT_DIR/$BINARY" "$MORPHEUS_BIN_DIR/$OUT_FILE"; then
    echo "Error: failed to install new binary, restoring previous version"
    mv "$MORPHEUS_BIN_DIR/$OUT_FILE.old" "$MORPHEUS_BIN_DIR/$OUT_FILE"
    exit 1
  fi
  rm -f "$MORPHEUS_BIN_DIR/$OUT_FILE.old"
else
  mv "$EXTRACT_DIR/$BINARY" "$MORPHEUS_BIN_DIR/$OUT_FILE"
fi

# The standalone uninstaller ships next to the main binary (morpheus::uninstall
# deletes BOTH images from the same directory).
if [ -f "$EXTRACT_DIR/morpheus-uninstall" ]; then
  mv -f "$EXTRACT_DIR/morpheus-uninstall" "$MORPHEUS_BIN_DIR/morpheus-uninstall"
fi

# slim-speak ships the voice daemon as a sibling of the main binary; the TUI
# spawns it from there, so it must be installed in the same directory.
if [ -f "$EXTRACT_DIR/morpheus-ttsd" ]; then
  mv -f "$EXTRACT_DIR/morpheus-ttsd" "$MORPHEUS_BIN_DIR/morpheus-ttsd"
fi

if [ "$OS" = "win32" ]; then
  for dll in "$EXTRACT_DIR"/*.dll; do
    if [ -f "$dll" ]; then
      mv "$dll" "$MORPHEUS_BIN_DIR/"
    fi
  done
fi

# Check PATH
if [[ ":$PATH:" != *":$MORPHEUS_BIN_DIR:"* ]]; then
  echo ""
  echo "Warning: morpheus installed, but $MORPHEUS_BIN_DIR is not in your PATH."
  echo ""
  SHELL_NAME=$(basename "$SHELL")
  echo "Add it by running:"
  echo "    echo 'export PATH=\"$MORPHEUS_BIN_DIR:\$PATH\"' >> ~/.${SHELL_NAME}rc"
  echo "Then reload your shell: source ~/.${SHELL_NAME}rc"
  echo ""
fi

echo "morpheus $MORPHEUS_VERSION installed successfully at $MORPHEUS_BIN_DIR/$OUT_FILE"
echo ""
echo "Run it with: morpheus"
