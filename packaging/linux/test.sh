#!/bin/sh
# Installs what build.sh made into a throwaway HOME, checks it, and uninstalls
# it again, checking that nothing but the person's data is left:
#
#   packaging/linux/test.sh OUT_DIR ARCH
#
# Covers the tarball and the single-file build. The .deb and .rpm are checked
# by the workflow, which has the package managers.
set -eu

NAME=droidtop-agent
BINS="droidtop-agent droidtop-agent-app"
SUMS=SHA256SUMS
DESKTOP_ID=dev.droidtop.agent

OUT=$(cd "${1:?OUT_DIR}" && pwd)
ARCH=${2:?ARCH}
T=$(mktemp -d)
http_pid=''
trap 'if [ -n "$http_pid" ]; then kill "$http_pid" 2>/dev/null || true; fi; rm -rf -- "${T:?}"' EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}
ok() { printf 'ok: %s\n' "$*"; }
fresh_home() {
    rm -rf -- "${T:?}/home"
    mkdir "$T/home"
    HOME=$T/home
    export HOME
    unset XDG_DATA_HOME XDG_CONFIG_HOME XDG_RUNTIME_DIR
}
# Nothing under HOME but (at most) the program's data, in its own folders.
only_data_left() {
    stray=$(find "$HOME" -mindepth 1 \( -type f -o -type l \) ! -path "$HOME/.config/$NAME/*" ! -path "$HOME/.local/share/$NAME/*" ! -path "$HOME/.config/windowcast/app/host/*")
    [ -z "$stray" ] || fail "$1: left behind: $stray"
}
nothing_left() {
    left=$(find "$HOME" -mindepth 1 \( -type f -o -type l \))
    [ -z "$left" ] || fail "$1: left behind: $left"
}

# ---- the tarball -------------------------------------------------------------
fresh_home
mkdir "$T/x"
zstd -dc "$OUT/$NAME-linux-$ARCH.tar.zst" | tar -xf - -C "$T/x"
pkg=$T/x/$NAME-linux-$ARCH
for f in install.sh uninstall.sh README.txt LICENSE VERSION share/applications/$DESKTOP_ID.desktop share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg; do
    [ -f "$pkg/$f" ] || fail "tarball lacks $f"
done
if grep -q '@GLIBC@' "$pkg/README.txt"; then fail "README.txt still has its placeholder"; fi

sh "$pkg/install.sh" --autostart
for b in $BINS; do [ -x "$HOME/.local/bin/$b" ] || fail "$b not installed"; done
manifest=$HOME/.local/share/$NAME/install-manifest
[ -f "$manifest" ] || fail "no manifest"
[ -x "$HOME/.local/share/$NAME/uninstall.sh" ] || fail "no uninstall.sh left beside the manifest"
grep -q "^Exec=$HOME/.local/bin/droidtop-agent-app\$" "$HOME/.local/share/applications/$DESKTOP_ID.desktop" || fail "desktop entry Exec not filled in"
[ -f "$HOME/.local/share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg" ] || fail "no icon"
autostart=$HOME/.config/autostart/$NAME.desktop
[ -f "$autostart" ] || fail "autostart did not leave $autostart"
grep -q "Exec=$HOME/.local/bin/droidtop-agent-app --hidden" "$autostart" || fail "autostart does not start the installed app"
while IFS="$(printf '\t')" read -r kind path; do
    case $kind in
    file) [ -e "$path" ] || fail "manifest lists $path but it is missing" ;;
    dir) [ -d "$path" ] || fail "manifest lists folder $path but it is missing" ;;
    esac
done <"$manifest"
"$HOME/.local/bin/droidtop-agent" --help >/dev/null || fail "droidtop-agent --help"
ok "tarball installs; manifest, desktop entry, icon and autostart are in place"

# Installing again over itself must not duplicate or lose entries.
sh "$pkg/install.sh" >/dev/null
[ "$(grep -c "^file	$HOME/.local/bin/droidtop-agent\$" "$manifest")" = 1 ] || fail "reinstall duplicated an entry"
grep -q "autostart" "$manifest" || fail "reinstall lost the autostart entry"
ok "reinstall keeps one entry each"

# A running agent is stopped by the uninstall; the program makes its data.
# (Looking for peers away from home is turned off first: this is a test.)
"$HOME/.local/bin/droidtop-agent" rendezvous off >/dev/null
"$HOME/.local/bin/droidtop-agent" run >"$T/agent.log" 2>&1 &
agent_pid=$!
n=0
while [ ! -d "$HOME/.local/share/$NAME" ] || [ ! -d "$HOME/.config/$NAME" ]; do
    n=$((n + 1))
    [ $n -lt 50 ] || fail "the agent did not start: $(cat "$T/agent.log")"
    sleep 0.2
done
kill -0 "$agent_pid" 2>/dev/null || fail "the agent ended: $(cat "$T/agent.log")"

sh "$HOME/.local/share/$NAME/uninstall.sh" >"$T/uninstall.log" || fail "uninstall failed: $(cat "$T/uninstall.log")"
cat "$T/uninstall.log"
if kill -0 "$agent_pid" 2>/dev/null; then fail "the agent is still running after uninstall"; fi
for b in $BINS; do [ ! -e "$HOME/.local/bin/$b" ] || fail "$b left in ~/.local/bin"; done
[ ! -e "$autostart" ] || fail "autostart entry left"
[ ! -e "$HOME/.local/share/applications/$DESKTOP_ID.desktop" ] || fail "desktop entry left"
[ ! -e "$manifest" ] || fail "manifest left"
only_data_left "uninstall"
[ -d "$HOME/.config/$NAME" ] || fail "uninstall deleted the data without --purge"
grep -q "Kept your data" "$T/uninstall.log" || fail "uninstall did not say it kept the data"
ok "uninstall stops the agent, removes its files and keeps the data"

sh "$pkg/install.sh" >/dev/null
sh "$HOME/.local/share/$NAME/uninstall.sh" --purge >/dev/null
nothing_left "purge"
if [ -e "$HOME/.config/$NAME" ] || [ -e "$HOME/.local/share/$NAME" ]; then fail "--purge left the data"; fi
ok "install, then uninstall --purge, leaves no files"

# The computer's identity is windowcast's: --purge keeps it while windowcast is installed.
fresh_home
mkdir -p "$HOME/.config/windowcast/app/host" "$HOME/.local/share/windowcast"
echo key >"$HOME/.config/windowcast/app/host/agent-identity.key"
echo manifest >"$HOME/.local/share/windowcast/install-manifest"
sh "$pkg/install.sh" >/dev/null
sh "$HOME/.local/share/$NAME/uninstall.sh" --purge >/dev/null
[ -f "$HOME/.config/windowcast/app/host/agent-identity.key" ] || fail "--purge deleted the identity windowcast shares"
[ ! -e "$HOME/.config/$NAME" ] || fail "--purge left the agent's settings"
ok "--purge keeps the identity windowcast shares"

# Pristine HOME: install then uninstall must leave not even folders.
fresh_home
sh "$pkg/install.sh" >/dev/null
sh "$HOME/.local/share/$NAME/uninstall.sh" >/dev/null
left=$(find "$HOME" -mindepth 1)
[ -z "$left" ] || fail "install then uninstall left: $left"
ok "install then uninstall leaves an empty HOME"

# --prefix, and the uninstall script run from the tarball
fresh_home
sh "$pkg/install.sh" --prefix "$T/prefix" >/dev/null
[ -x "$T/prefix/bin/droidtop-agent" ] || fail "--prefix install"
[ -f "$T/prefix/share/$NAME/install-manifest" ] || fail "--prefix manifest"
sh "$pkg/uninstall.sh" --prefix "$T/prefix" >/dev/null
[ ! -e "$T/prefix/bin/droidtop-agent" ] || fail "--prefix uninstall"
nothing_left "prefix"
ok "--prefix install and uninstall"

# ---- install.sh and uninstall.sh downloaded alone (curl | sh) ----------------
# A folder served on loopback stands in for a release.
fresh_home
mkdir "$T/alone" "$T/site"
cp "$OUT/install.sh" "$OUT/uninstall.sh" "$T/alone/"
cp "$OUT/$NAME-linux-$ARCH.tar.zst" "$T/site/"
(cd "$T/site" && sha256sum -- "$NAME-linux-$ARCH.tar.zst" >"$SUMS")
port=$((20000 + $$ % 20000))
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$T/site" >/dev/null 2>&1 &
http_pid=$!
n=0
until curl -fsS -o /dev/null "http://127.0.0.1:$port/$SUMS" 2>/dev/null; do
    n=$((n + 1))
    [ $n -lt 50 ] || fail "the local download server did not start"
    sleep 0.2
done
(cd "$T/alone" && sh ./install.sh --base-url "http://127.0.0.1:$port" --autostart) >"$T/alone.log" 2>&1 || fail "download install failed: $(cat "$T/alone.log")"
grep -q "Checked against $SUMS" "$T/alone.log" || fail "the download was not checked against $SUMS"
for b in $BINS; do [ -x "$HOME/.local/bin/$b" ] || fail "download install lacks $b"; done
(cd "$T/alone" && sh ./uninstall.sh) >/dev/null
only_data_left "download install"
echo "0000000000000000000000000000000000000000000000000000000000000000  $NAME-linux-$ARCH.tar.zst" >"$T/site/$SUMS"
if (cd "$T/alone" && sh ./install.sh --base-url "http://127.0.0.1:$port") >"$T/bad.log" 2>&1; then fail "a download that does not match its sum was installed"; fi
grep -q "does not match" "$T/bad.log" || fail "no complaint about the wrong sum: $(cat "$T/bad.log")"
[ ! -e "$HOME/.local/bin/${BINS%% *}" ] || fail "a download that does not match its sum left a program behind"
kill "$http_pid" 2>/dev/null || true
ok "install.sh and uninstall.sh work downloaded alone; a wrong sum installs nothing"

# ---- the single file ---------------------------------------------------------
run=$OUT/$NAME-linux-$ARCH.run
[ -x "$run" ] || fail "$run is not executable"
fresh_home
"$run" help | grep -q "droidtop-agent in one file" || fail ".run help"
# Without FUSE, unpacked first (what a machine with no FUSE does by itself).
RUNIMAGE_EXTRACT_AND_RUN=1 "$run" help >/dev/null 2>&1 || fail ".run with extraction"
ok ".run starts and shows its help"
"$run" agent --help | grep -q "droidtop-agent" || fail ".run agent --help"
"$run" install --autostart >/dev/null
for b in $BINS; do [ -x "$HOME/.local/bin/$b" ] || fail ".run did not install $b"; done
[ -f "$HOME/.config/autostart/$NAME.desktop" ] || fail ".run --autostart"
"$HOME/.local/share/$NAME/uninstall.sh" --purge >/dev/null
nothing_left ".run uninstall"
ok ".run installs and uninstalls through the same scripts"

printf 'all passed\n'
