#!/bin/bash
set -euo pipefail

export GIT_TERMINAL_PROMPT=0

if ! command -v gh >/dev/null 2>&1; then
  echo "error: github cli (gh) not installed" >&2
  exit 2
fi
if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  echo "error: github cli (gh) not authenticated" >&2
  exit 3
fi

if [ ! -f /etc/os-release ]; then
  echo "error: /etc/os-release not found" >&2
  exit 4
fi

. /etc/os-release

case "$ID" in
  debian|ubuntu)
    DEBIAN_FRONTEND=noninteractive apt-get update -y
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
      build-essential \
      pkg-config \
      libssl-dev \
      libcap-dev \
      python3-pytest \
      just \
      curl
    ;;
  *)
    echo "error: unsupported distro for automatic long-horizon prerequisites install: $ID" >&2
    exit 5
    ;;
esac

if ! command -v node >/dev/null 2>&1 || ! node --version 2>/dev/null | grep -Eq '^v22\.'; then
  NODE_VER='v22.22.0'
  ARCHIVE="node-${NODE_VER}-linux-x64.tar.xz"
  URL="https://nodejs.org/dist/${NODE_VER}/${ARCHIVE}"
  TMP="/tmp/${ARCHIVE}"
  DEST="/usr/local/node-${NODE_VER}-linux-x64"
  curl --fail --silent --show-error --connect-timeout 10 --max-time 300 -o "$TMP" "$URL"
  rm -rf "$DEST"
  mkdir -p /usr/local
  tar -xJf "$TMP" -C /usr/local
  ln -sf "$DEST/bin/node" /usr/local/bin/node
  ln -sf "$DEST/bin/npm" /usr/local/bin/npm
  ln -sf "$DEST/bin/npx" /usr/local/bin/npx
fi

printf 'Installed long-horizon prerequisites:\n'
printf ' - just: %s\n' "$(just --version)"
printf ' - pytest: %s\n' "$(python3 -m pytest --version)"
printf ' - node: %s\n' "$(node --version)"
