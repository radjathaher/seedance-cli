#!/usr/bin/env bash
set -euo pipefail
version="${SEEDANCE_CLI_VERSION:-0.2.0}"
os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
  Darwin-arm64|Darwin-aarch64) target="darwin-aarch64" ;;
  Linux-x86_64|Linux-amd64) target="linux-x86_64" ;;
  *) echo "unsupported platform: $os-$arch" >&2; exit 1 ;;
esac
url="https://github.com/radjathaher/seedance-cli/releases/download/v${version}/seedance-cli-${version}-${target}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp/seedance.tar.gz"
tar -xzf "$tmp/seedance.tar.gz" -C "$tmp"
mkdir -p "${HOME}/.local/bin"
install -m 0755 "$tmp/seedance" "${HOME}/.local/bin/seedance"
echo "installed ${HOME}/.local/bin/seedance"
