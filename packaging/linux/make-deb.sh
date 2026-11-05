#!/usr/bin/env bash
# Wrap a built bliss in a .deb: apt then brings the libraries it runs on, and
# installing it gives it the capability TUN needs, which `setcap` would
# otherwise have to be run for by hand after every update (AGENTS.md §9).
#
#   packaging/linux/make-deb.sh <path to bliss> <version> <output .deb>
set -euo pipefail

binary="$1"
version="${2#v}"
out="$3"

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
chmod 755 "$root"

install -Dm755 "$binary" "$root/usr/bin/bliss"
mkdir -p "$root/DEBIAN"

# The runtime libraries only: the window's WebKitGTK and GTK; the tray's
# library is looked for at run time and bliss runs without it (AGENTS.md
# §6 rule 27), so it is recommended, not required; libcap2-bin is setcap.
cat >"$root/DEBIAN/control" <<EOF
Package: bliss
Version: ${version}
Section: net
Priority: optional
Architecture: amd64
Maintainer: Maestix <noreply@users.noreply.github.com>
Depends: libwebkit2gtk-4.1-0, libgtk-3-0, libsoup-3.0-0, libc6 (>= 2.35)
Recommends: libayatana-appindicator3-1, libcap2-bin
Suggests: openvpn
Description: proxy client: VLESS, VMess, Trojan, Shadowsocks, Hysteria 2, WireGuard
 Drives xray-core, routes by rules, sets the desktop's proxy and, with
 TUN, carries the whole machine.
EOF

# TUN needs CAP_NET_ADMIN, and a new file never has it.
cat >"$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v setcap >/dev/null 2>&1; then
    setcap cap_net_admin+ep /usr/bin/bliss || true
fi
EOF
chmod 755 "$root/DEBIAN/postinst"

dpkg-deb --root-owner-group --build "$root" "$out" >/dev/null
echo "$out"
