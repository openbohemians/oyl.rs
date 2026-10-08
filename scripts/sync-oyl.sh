#!/bin/sh
# Vendor Oyl's C sources into oyl-sys/oyl, from a checkout's committed HEAD,
# recording the commit in oyl-sys/oyl/COMMIT.
#
#   scripts/sync-oyl.sh path/to/oyl
set -eu
src=${1:?usage: scripts/sync-oyl.sh path/to/oyl}
dest="$(cd "$(dirname "$0")/.." && pwd)/oyl-sys/oyl"
rm -rf "$dest"
mkdir -p "$dest"
git -C "$src" archive HEAD src include LICENSE | tar -x -C "$dest"
git -C "$src" log -1 --format='%H %cs %s' > "$dest/COMMIT"
echo "vendored: $(cat "$dest/COMMIT")"
