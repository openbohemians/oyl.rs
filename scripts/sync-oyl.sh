#!/bin/sh
# Vendor Oyl's C sources into oyl-sys/oyl from a checkout, at its committed
# HEAD or at the given ref (a release tag), recording the commit in
# oyl-sys/oyl/COMMIT and Oyl's version in the build metadata of oyl-sys's
# version (0.1.0+1.0.0).
#
#   scripts/sync-oyl.sh path/to/oyl [ref]
set -eu
src=${1:?usage: scripts/sync-oyl.sh path/to/oyl [ref]}
ref=${2:-HEAD}
dest="$(cd "$(dirname "$0")/.." && pwd)/oyl-sys/oyl"
rm -rf "$dest"
mkdir -p "$dest"
git -C "$src" archive "$ref" src include LICENSE | tar -x -C "$dest"
git -C "$src" log -1 --format='%H %cs %s' "$ref" > "$dest/COMMIT"
v=$(sed -n 's/^#define OYL_VERSION_\(MAJOR\|MINOR\|PATCH\) \([0-9]*\)$/\2/p' \
    "$dest/include/oyl/oyl.h" | paste -sd.)
sed -i -E "s/^(version = \"[0-9]+\.[0-9]+\.[0-9]+)(\+[^\"]*)?\"/\1+$v\"/" \
    "$dest/../Cargo.toml"
echo "vendored: $(cat "$dest/COMMIT") (Oyl $v)"
