#!/usr/bin/env bash
# The agent classifies folders with copies of droidtop-platforms' platforms and engines databases (crates/scan/data), so the
# computer and the handheld say the same about a folder (droidtop SPEC 7e2b). crates/scan/data/UPSTREAM names the
# droidtop-platforms commit the copies are. This fails when a copy is not that commit's file, so a hand edit, or a copy
# refreshed without moving the pin, cannot pass. To refresh: copy both files from the new commit, write its hash in UPSTREAM.
set -euo pipefail
cd "$(dirname "$0")/.."
sha="$(tr -d '[:space:]' < crates/scan/data/UPSTREAM)"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "crates/scan/data/UPSTREAM must hold a full commit hash" >&2; exit 1; }
status=0
for file in platforms-database.json engines-database.json; do
    upstream="$(mktemp)"
    curl -fsS --retry 3 -o "$upstream" "https://raw.githubusercontent.com/Droidtop/droidtop-platforms/$sha/$file"
    if cmp -s "$upstream" "crates/scan/data/$file"; then
        echo "$file is droidtop-platforms' at $sha"
    else
        echo "crates/scan/data/$file is not droidtop-platforms' $file at $sha" >&2
        status=1
    fi
    rm -f "$upstream"
done
exit $status
