#!/usr/bin/env bash
# Builds DotCraft in release mode and packages it as a Debian/Ubuntu package:
#   target/release/dotcraft_<version>_<arch>.deb
# Options:
#   --no-build   package the existing target/release/dotcraft
#   --install    install the package with apt afterwards (asks for sudo)
# Needs dpkg-deb (Debian, Ubuntu and derivatives).
set -euo pipefail
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true

BUILD=1 INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --install) INSTALL=1 ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(dpkg --print-architecture)
if (( BUILD )); then
  cargo build --locked --release -p rc-app
fi
BIN=target/release/dotcraft
[[ -x $BIN ]] || { echo "$BIN not found; build it first" >&2; exit 1; }

PKG=target/deb/dotcraft_${VERSION}_${ARCH}
rm -rf "$PKG"
install -Dm755 "$BIN" "$PKG/usr/bin/dotcraft"
install -Dm644 assets/dotcraft.desktop "$PKG/usr/share/applications/dotcraft.desktop"
install -Dm644 assets/icon-32.png "$PKG/usr/share/icons/hicolor/32x32/apps/dotcraft.png"
install -Dm644 assets/icon-64.png "$PKG/usr/share/icons/hicolor/64x64/apps/dotcraft.png"
install -Dm644 assets/icon.png "$PKG/usr/share/icons/hicolor/512x512/apps/dotcraft.png"
install -Dm644 assets/icon.svg "$PKG/usr/share/icons/hicolor/scalable/apps/dotcraft.svg"
install -d "$PKG/usr/share/doc/dotcraft"
{
  echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
  echo "Upstream-Name: DotCraft"
  echo "Source: https://github.com/javy-fernandez/dotcraft"
  echo
  echo "Files: *"
  echo "License: MIT or Apache-2.0"
  echo
  sed 's/^/ /; s/^ $/ ./' LICENSE-MIT
  echo
  echo "Icons in the binary come from the IntelliJ Platform (Apache-2.0); see /usr/share/common-licenses/Apache-2.0."
  echo
  echo "Files: crates/rc-sql (libpg_query, PostgreSQL's parser)"
  echo "License: BSD-3-Clause and PostgreSQL"
  sed 's/^/ /; s/^ $/ ./' docs/third-party/libpg_query-LICENSE.txt
} > "$PKG/usr/share/doc/dotcraft/copyright"

# The binary links only glibc and libgcc; windowing and GPU libraries are loaded at runtime.
SIZE=$(du -sk "$PKG/usr" | cut -f1)
install -d "$PKG/DEBIAN"
cat > "$PKG/DEBIAN/control" <<CONTROL
Package: dotcraft
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: DotCraft maintainers <dotcraft@users.noreply.github.com>
Installed-Size: ${SIZE}
Depends: libc6 (>= 2.35), libgcc-s1, libxkbcommon0
Recommends: libvulkan1 | libegl1, libwayland-client0, libxkbcommon-x11-0, libx11-6, libxcursor1, libxrandr2, libxi6, git
Suggests: dotnet-sdk-10.0 | dotnet-sdk-9.0 | dotnet-sdk-8.0
Section: devel
Priority: optional
Homepage: https://github.com/javy-fernandez/dotcraft
Description: Lightweight native IDE for .NET and C#
 Solutions, Roslyn code intelligence, build, run, tests, debugging and Git
 in a fast desktop app built with Rust and egui. C# files can be formatted
 with Roslyn or CSharpier.
CONTROL

cat > "$PKG/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
  command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/share/applications || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
fi
POSTINST
cp "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"
sed -i 's/= configure/= remove/' "$PKG/DEBIAN/postrm"
chmod 755 "$PKG/DEBIAN/postinst" "$PKG/DEBIAN/postrm"

OUT=target/release/dotcraft_${VERSION}_${ARCH}.deb
dpkg-deb --root-owner-group --build "$PKG" "$OUT" >/dev/null
echo "$OUT"
dpkg-deb --info "$OUT" | sed -n '/Package:/,/Suggests:/p'

if (( INSTALL )); then
  sudo apt-get install -y "./$OUT"
fi
