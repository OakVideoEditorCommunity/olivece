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

# Build the .rpm: stage into a buildroot and let rpmbuild's automatic
# dependency discovery (find-requires) compute the Requires from the
# binaries' NEEDED entries. Run from the repo root after
# `cargo build --release`. Usage: tooling/package/build-rpm.sh <version>
set -euo pipefail

VERSION="${1:?usage: build-rpm.sh <version>}"
TOP=$(pwd)/target/pkg/rpm
rm -rf "$TOP"
mkdir -p "$TOP"/{BUILD,RPMS,SOURCES,SPECS,BUILDROOT}

ROOT="$TOP/BUILDROOT/oak-editor-$VERSION-1.x86_64"
mkdir -p "$ROOT/usr/bin" "$ROOT/usr/share/applications" \
	"$ROOT/usr/share/icons/hicolor/512x512/apps" "$ROOT/usr/share/oak/i18n" \
	"$ROOT/usr/share/icons/hicolor/scalable/apps"
install -m755 target/release/oak-editor target/release/oak-cli target/release/oak-worker \
	"$ROOT/usr/bin/"
install -m644 packaging/oak.desktop "$ROOT/usr/share/applications/oak.desktop"
install -m644 icons/icon.png "$ROOT/usr/share/icons/hicolor/512x512/apps/oak.png"
install -m644 Oak_Icon.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/oak.svg"
install -m644 assets/i18n/*.yaml "$ROOT/usr/share/oak/i18n/"

rpmbuild -bb \
	--define "_topdir $TOP" \
	--define "_version $VERSION" \
	--define "buildroot $ROOT" \
	--define "_binary_payload w6.zstdio" \
	tooling/package/oak.spec

find "$TOP/RPMS" -name '*.rpm' -exec mv {} target/release/ \;
ls target/release/*.rpm
