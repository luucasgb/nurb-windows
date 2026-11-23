#!/bin/sh
# Stages what the desktop app bundles for first-launch provisioning: the nurb
# wheel built from this checkout, its dependencies as pinned by uv.lock, the
# committed adapter manifest/lock, and the uv sidecar binaries for both darwin
# targets (or for x86_64 Windows, when run from Git Bash on Windows). Runs before every tauri dev/build (wheel and Python lock are cheap and
# must track the checkout); the uv downloads are skipped once present.
#
# The lock's bytes decide whether an installed app keeps its environment, so they
# depend on uv.lock and nothing else: no header naming this checkout's path, and
# no fresh resolve that picks up whatever is newest on the day of the build.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
tauri="$here/../src-tauri"
repo="$here/../../engine"
adapter_runtime="$here/../adapter-runtime"
UV_VERSION=0.12.1

mkdir -p "$tauri/resources" "$tauri/binaries"

rm -f "$tauri/resources"/nurb-*.whl
uv build --wheel --project "$repo" -o "$tauri/resources" >/dev/null 2>&1
uv export --project "$repo" --locked --no-dev --no-emit-project --no-header \
  --no-annotate --format requirements-txt -q -o "$tauri/resources/requirements.lock"
cp "$adapter_runtime/package.json" "$tauri/resources/adapter-package.json"
cp "$adapter_runtime/package-lock.json" "$tauri/resources/adapter-package-lock.json"

for triple in aarch64-apple-darwin x86_64-apple-darwin; do
  # A Windows build (Git Bash) bundles the Windows uv instead; see below.
  [ -n "${WINDIR:-}" ] && break
  out="$tauri/binaries/uv-$triple"
  [ -x "$out" ] && continue
  echo "stage: downloading uv $UV_VERSION for $triple"
  tmp="$(mktemp -d)"
  base="https://github.com/astral-sh/uv/releases/download/$UV_VERSION"
  curl -fsSL "$base/uv-$triple.tar.gz" -o "$tmp/uv.tar.gz"
  curl -fsSL "$base/uv-$triple.tar.gz.sha256" -o "$tmp/uv.tar.gz.sha256"
  (cd "$tmp" && printf '%s  uv.tar.gz\n' "$(cut -d' ' -f1 uv.tar.gz.sha256)" | shasum -a 256 -c - >/dev/null)
  tar -xzf "$tmp/uv.tar.gz" -C "$tmp"
  found="$(find "$tmp" -type f -name uv | head -1)"
  [ -n "$found" ] || { echo "stage: uv binary not found in tarball" >&2; exit 1; }
  mv "$found" "$out"
  chmod +x "$out"
  rm -rf "$tmp"
done

# Windows: the same pinned uv, shipped as a zip with an .exe inside.
if [ -n "${WINDIR:-}" ]; then
  triple=x86_64-pc-windows-msvc
  out="$tauri/binaries/uv-$triple.exe"
  if [ ! -f "$out" ]; then
    echo "stage: downloading uv $UV_VERSION for $triple"
    tmp="$(mktemp -d)"
    base="https://github.com/astral-sh/uv/releases/download/$UV_VERSION"
    curl -fsSL "$base/uv-$triple.zip" -o "$tmp/uv.zip"
    curl -fsSL "$base/uv-$triple.zip.sha256" -o "$tmp/uv.zip.sha256"
    (cd "$tmp" && printf '%s  uv.zip\n' "$(cut -d' ' -f1 uv.zip.sha256)" | sha256sum -c - >/dev/null)
    unzip -q "$tmp/uv.zip" -d "$tmp"
    found="$(find "$tmp" -type f -name uv.exe | head -1)"
    [ -n "$found" ] || { echo "stage: uv.exe not found in zip" >&2; exit 1; }
    mv "$found" "$out"
    rm -rf "$tmp"
  fi
fi
