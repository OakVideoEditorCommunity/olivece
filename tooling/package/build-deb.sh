#!/bin/bash
# Oak Video Editor - Non-Linear Video Editor
# Copyright (C) 2026 Oak Team
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.

# Build the .deb by hand: stage the release binaries + resources, compute
# the FULL runtime dependency set with dpkg-shlibdeps (Debian-family names
# of the build distro), and pack with dpkg-deb. Run from the repo root
# after `cargo build --release`. Usage: tooling/package/build-deb.sh <version>
set -euo pipefail

VERSION="${1:?usage: build-deb.sh <version>}"
STAGING=target/pkg/deb
rm -rf "$STAGING"
mkdir -p "$STAGING/usr/bin" "$STAGING/usr/share/applications" \
	"$STAGING/usr/share/icons/hicolor/512x512/apps" "$STAGING/usr/share/oak/i18n" \
	"$STAGING/usr/share/icons/hicolor/scalable/apps" "$STAGING/DEBIAN"

install -m755 target/release/oak-editor target/release/oak-cli target/release/oak-worker \
	"$STAGING/usr/bin/"
install -m644 packaging/oak.desktop "$STAGING/usr/share/applications/oak.desktop"
install -m644 icons/icon.png "$STAGING/usr/share/icons/hicolor/512x512/apps/oak.png"
install -m644 Oak_Icon.svg "$STAGING/usr/share/icons/hicolor/scalable/apps/oak.svg"
install -m644 assets/i18n/*.yaml "$STAGING/usr/share/oak/i18n/"

# The full shlib dependency set (FFmpeg/OCIO are statically linked, so
# only base-OS packages appear).
DEPS=$(for bin in "$STAGING"/usr/bin/*; do dpkg-shlibdeps -O "$bin"; done \
	| sed 's/^shlibs:Depends=//' | tr ',' '\n' | sed 's/^ //;s/ $//' | sort -u \
	| paste -sd', ' -)
echo "declared deps: $DEPS"

cat > "$STAGING/DEBIAN/control" <<EOF
Package: oak-editor
Version: $VERSION
Section: video
Priority: optional
Architecture: $(dpkg --print-architecture)
Maintainer: Oak Team
Depends: $DEPS
Description: Oak Video Editor — a free, open-source non-linear video editor
 Oak is a non-linear video editor written in Rust (OpenFX plug-in host,
 proxy editing, multicam, hardware decoding).
EOF

dpkg-deb --root-owner-group --build "$STAGING" "target/release/oak-editor_${VERSION}_amd64.deb"
echo "built target/release/oak-editor_${VERSION}_amd64.deb"
