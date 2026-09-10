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

# RPM spec for Oak. Requires are AUTO-COMPUTED by rpmbuild from the
# packaged binaries' NEEDED entries (the distro's own names); do not add a
# static Requires list.
Name:           oak-editor
Version:        %{_version}
Release:        1%{?dist}
Summary:        Oak Video Editor — a free, open-source non-linear video editor
License:        GPL-3.0-or-later
URL:            https://github.com/OakVideoEditorCommunity/oak

%description
Oak is a non-linear video editor written in Rust (OpenFX plug-in host,
proxy editing, multicam, hardware decoding).

%install
# Everything is staged into %{buildroot} by the caller (build-rpm.sh);
# nothing to compile here.
true

%files
/usr/bin/oak-editor
/usr/bin/oak-cli
/usr/bin/oak-worker
/usr/share/applications/oak.desktop
/usr/share/icons/hicolor/512x512/apps/oak.png
/usr/share/icons/hicolor/scalable/apps/oak.svg
/usr/share/oak/i18n/

%changelog
