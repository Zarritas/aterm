#!/usr/bin/env bash
# Build a .vsix that bundles the `agent-sessions-cli` sidecar for a single
# target platform. Multi-platform releases are built by running this once per
# target on the appropriate host (or via cross-compile).
#
# Usage:
#   ./scripts/build-vsix.sh                 # auto-detects the host platform
#   ./scripts/build-vsix.sh linux-x64       # explicit VSCE target
#   ./scripts/build-vsix.sh darwin-arm64
#   ./scripts/build-vsix.sh win32-x64
#
# Output: ./agent-sessions-<version>-<vsce-target>.vsix at the extension root.
#
# The script:
#   1. Picks the rustc target triple that matches the VSCE target.
#   2. Builds `agent-sessions-cli` in release mode for that triple.
#   3. Copies the binary into `bin/<rust-triple>/agent-sessions-cli[.exe]`.
#      The extension auto-resolves this path at runtime.
#   4. Compiles the TypeScript and runs `vsce package --target <vsce-target>`.

set -euo pipefail

cd "$(dirname "$0")/.."
EXT_ROOT="$(pwd)"
REPO_ROOT="$(cd "$EXT_ROOT/.." && pwd)"

# ── Pick the VSCE target ─────────────────────────────────────────────────────
detect_host_vsce_target() {
  local os arch
  case "$(uname -s)" in
    Linux)   os=linux ;;
    Darwin)  os=darwin ;;
    MINGW*|MSYS*|CYGWIN*) os=win32 ;;
    *) echo "OS no soportado: $(uname -s)" >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=x64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) echo "Arquitectura no soportada: $(uname -m)" >&2; exit 1 ;;
  esac
  echo "${os}-${arch}"
}

VSCE_TARGET="${1:-$(detect_host_vsce_target)}"

# Map VSCE target → rustc triple (also used as the bin/ subdir name).
case "$VSCE_TARGET" in
  linux-x64)    RUST_TRIPLE="x86_64-unknown-linux-gnu" ;;
  linux-arm64)  RUST_TRIPLE="aarch64-unknown-linux-gnu" ;;
  darwin-x64)   RUST_TRIPLE="x86_64-apple-darwin" ;;
  darwin-arm64) RUST_TRIPLE="aarch64-apple-darwin" ;;
  win32-x64)    RUST_TRIPLE="x86_64-pc-windows-msvc" ;;
  win32-arm64)  RUST_TRIPLE="aarch64-pc-windows-msvc" ;;
  *)
    echo "Target VSCE desconocido: $VSCE_TARGET" >&2
    echo "Soportados: linux-x64, linux-arm64, darwin-x64, darwin-arm64, win32-x64, win32-arm64" >&2
    exit 1
    ;;
esac

EXE_SUFFIX=""
[[ "$VSCE_TARGET" == win32-* ]] && EXE_SUFFIX=".exe"

echo "→ Build sidecar para $RUST_TRIPLE ($VSCE_TARGET)"
(cd "$REPO_ROOT" && cargo build --release -p agent-sessions-cli --target "$RUST_TRIPLE")

BIN_DEST="$EXT_ROOT/bin/$RUST_TRIPLE"
mkdir -p "$BIN_DEST"
cp "$REPO_ROOT/target/$RUST_TRIPLE/release/agent-sessions-cli${EXE_SUFFIX}" \
   "$BIN_DEST/agent-sessions-cli${EXE_SUFFIX}"
chmod +x "$BIN_DEST/agent-sessions-cli${EXE_SUFFIX}" 2>/dev/null || true
echo "  $BIN_DEST/agent-sessions-cli${EXE_SUFFIX}"

echo "→ Compilar TypeScript"
npm run compile >/dev/null

echo "→ Empaquetar .vsix"
# `vsce` lives at @vscode/vsce. Falls back to a globally installed binary.
if [[ -x "$EXT_ROOT/node_modules/.bin/vsce" ]]; then
  VSCE="$EXT_ROOT/node_modules/.bin/vsce"
else
  VSCE="$(command -v vsce || true)"
fi
if [[ -z "$VSCE" ]]; then
  echo "vsce no encontrado. Instálalo con: npm i -D @vscode/vsce" >&2
  exit 1
fi
"$VSCE" package --target "$VSCE_TARGET" --no-dependencies

echo "✓ Listo. Resultado:"
ls -1 *.vsix
