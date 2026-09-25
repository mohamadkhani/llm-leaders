#!/usr/bin/env bash
# Build a Debian / Ubuntu (.deb) package for llm-leaders.
#
# Usage:
#   packaging/debian/build-deb.sh <version> [binary_path] [output_dir] [arch]
#
# Example:
#   packaging/debian/build-deb.sh 0.8.0 target/release/llm-leaders dist amd64

set -euo pipefail

VERSION="${1:?Version argument is required (e.g. 0.8.0)}"
BINARY="${2:-target/release/llm-leaders}"
OUTDIR="${3:-dist}"
ARCH="${4:-amd64}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

if [ ! -f "$BINARY" ]; then
  echo "Error: Binary not found at '$BINARY'" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
OUTDIR="$(cd "$OUTDIR" && pwd)"
DEB_NAME="llm-leaders_${VERSION}_${ARCH}.deb"
DEB_PATH="${OUTDIR}/${DEB_NAME}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PKG_DIR="${WORK}/pkg"
mkdir -p "${PKG_DIR}/DEBIAN"
mkdir -p "${PKG_DIR}/usr/bin"
mkdir -p "${PKG_DIR}/usr/share/doc/llm-leaders"

# Copy binary with executable permissions
install -m 755 "$BINARY" "${PKG_DIR}/usr/bin/llm-leaders"

# Copy license and readme if present
if [ -f "${REPO_ROOT}/COPYING" ]; then
  install -m 644 "${REPO_ROOT}/COPYING" "${PKG_DIR}/usr/share/doc/llm-leaders/copyright"
fi
if [ -f "${REPO_ROOT}/README.md" ]; then
  install -m 644 "${REPO_ROOT}/README.md" "${PKG_DIR}/usr/share/doc/llm-leaders/README.md"
fi

# Generate control file from template
CONTROL_SRC="${SCRIPT_DIR}/control"
if [ ! -f "$CONTROL_SRC" ]; then
  echo "Error: Control template not found at '$CONTROL_SRC'" >&2
  exit 1
fi

sed -e "s/^Version:.*/Version: ${VERSION}/" \
    -e "s/^Architecture:.*/Architecture: ${ARCH}/" \
    "$CONTROL_SRC" > "${PKG_DIR}/DEBIAN/control"

# Compute Installed-Size in KB
INSTALLED_SIZE="$(du -sk "${PKG_DIR}/usr" | awk '{print $1}')"
echo "Installed-Size: ${INSTALLED_SIZE}" >> "${PKG_DIR}/DEBIAN/control"

# Generate md5sums for integrity verification
(
  cd "$PKG_DIR"
  find usr -type f -exec md5sum {} +
) > "${PKG_DIR}/DEBIAN/md5sums"
chmod 644 "${PKG_DIR}/DEBIAN/md5sums"

# Build the .deb
if command -v dpkg-deb >/dev/null 2>&1; then
  echo "Building .deb using dpkg-deb..."
  dpkg-deb --build --root-owner-group "$PKG_DIR" "$DEB_PATH"
else
  echo "dpkg-deb not found; assembling .deb with ar/tar..."
  # Debian binary signature
  echo "2.0" > "${WORK}/debian-binary"

  # Control tarball (tar with owner root:root)
  tar --owner=0 --group=0 --numeric-owner -czf "${WORK}/control.tar.gz" -C "${PKG_DIR}/DEBIAN" .

  # Data tarball
  tar --owner=0 --group=0 --numeric-owner -czf "${WORK}/data.tar.gz" -C "$PKG_DIR" usr

  (
    cd "$WORK"
    ar -rc "$DEB_PATH" debian-binary control.tar.gz data.tar.gz
  )
fi

echo "Successfully built: ${DEB_PATH}"
if command -v dpkg-deb >/dev/null 2>&1; then
  echo "--- Package Info ---"
  dpkg-deb -I "$DEB_PATH"
  echo "--- Package Contents ---"
  dpkg-deb -c "$DEB_PATH"
fi
