#!/bin/sh
# Builds what the AWS server runs, on Amazon Linux 2023 (the server's own system, so
# its embedded Python matches): the pinned Crossfire C server with its arch and maps
# in /opt/crossfire, and the gateway. Writes dist/crossfir3d-server-<arch>.tar.gz.
# Run in an amazonlinux:2023 container from the repo root (GitHub Actions does).
set -eu

ROOT=$(pwd)
. deploy/pins.env
SF=https://git.code.sf.net/p/crossfire
PREFIX=/opt/crossfire
WORK=$(mktemp -d)

dnf install -y -q gcc gcc-c++ make autoconf automake libtool flex pkgconf-pkg-config \
    sqlite-devel python3-devel python3-pip git tar gzip which findutils shadow-utils util-linux
if ! command -v cargo >/dev/null; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal
    . "$HOME/.cargo/env"
fi

fetch() { # repo pin
    git clone -q "$SF/$1" "$WORK/$1"
    git -C "$WORK/$1" checkout -q "$2"
}
fetch crossfire-server "$SERVER_PIN"
fetch crossfire-arch "$ARCH_PIN"
fetch crossfire-maps "$MAPS_PIN"
ln -s "$WORK/crossfire-arch" "$WORK/crossfire-server/lib/arch"
# The build runs the server once, which needs the maps too.
ln -s "$WORK/crossfire-maps" "$WORK/crossfire-server/lib/maps"

# The server's build runs it (to compile the arch), and it refuses to run as root.
id builder >/dev/null 2>&1 || useradd -m builder
mkdir -p "$PREFIX" /srv/crossfire
chown -R builder "$WORK" "$PREFIX" /srv/crossfire
as_builder() { runuser -u builder -- sh -c "cd '$WORK/crossfire-server' && $1"; }

cd "$WORK/crossfire-server"
# Upstream never saves a character with no experience, so a new character is gone
# (and missing from its account) after the next run. Save it anyway.
sed -i 's|return 0; /\* no experience, no save \*/|; /* Crossfir3d: save new characters too */|' server/login.cpp
grep -q 'Crossfir3d: save new characters' server/login.cpp
# No libcurl: no metaserver, so the server never announces itself.
as_builder "sh ./autogen.sh --prefix=$PREFIX --localstatedir=/srv --without-libcurl >/dev/null"
as_builder "make -s -j$(nproc) && make -s install"
# The maps as they are at the pin, without git history.
rm -rf "$PREFIX/share/crossfire/maps"
mkdir -p "$PREFIX/share/crossfire/maps"
git -c safe.directory="$WORK/crossfire-maps" -C "$WORK/crossfire-maps" archive HEAD | tar -x -C "$PREFIX/share/crossfire/maps"
# Python modules the map scripts import; python-cjson (in the maps' requirements) is
# Python 2 only, so a stand-in over the stdlib json module replaces it.
python3 -m pip install -q --target "$PREFIX/pylib" requests
printf 'from json import loads as decode, dumps as encode\n' > "$PREFIX/pylib/cjson.py"
# The starting runtime data: install.sh copies it to the data volume the first time.
mv /srv/crossfire "$PREFIX/var-template"

cd "$ROOT"
cargo build --release --locked -q -p gateway
mkdir -p "$PREFIX/bin" dist
cp target/release/gateway "$PREFIX/bin/"
cp -r deploy/server "$PREFIX/deploy"
tar -czf "dist/crossfir3d-server-$(uname -m).tar.gz" --owner=0 --group=0 --numeric-owner -C / "${PREFIX#/}"
echo "built dist/crossfir3d-server-$(uname -m).tar.gz"
