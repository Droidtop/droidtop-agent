#!/bin/sh
# Installs droidtop-agent for the current user. No root and no package manager.
#
#   sh install.sh [options]
#
# Run it from an unpacked droidtop-agent-linux-<arch>.tar.zst, or download it
# alone (it then fetches the tarball for this machine from the latest release
# and checks it against the release's SHA256SUMS):
#
#   curl -fsSL https://github.com/Droidtop/droidtop-agent/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --autostart
#
# Everything it creates is listed in ~/.local/share/droidtop-agent/install-manifest,
# and uninstall.sh (copied there too) removes exactly that.
set -eu

# ---- this project ---------------------------------------------------------
NAME=droidtop-agent
REPO=Droidtop/droidtop-agent
BINS="droidtop-agent droidtop-agent-app"
DESKTOP_ID=dev.droidtop.agent
# ---------------------------------------------------------------------------

TAB=$(printf '\t')

usage() {
    cat <<'EOF'
Usage: install.sh [options]

Installs droidtop-agent and droidtop-agent-app for the current user.

  --prefix DIR     install under DIR/bin and DIR/share instead of ~/.local
  --autostart      also start the agent when you sign in
                   (runs `droidtop-agent autostart on`)
  --no-desktop     skip the menu entry and the icon
  --from DIR       use an unpacked tarball in DIR
  --tarball FILE   use this .tar.zst instead of downloading one
  --release TAG    download that release (default: the latest)
  --base-url URL   download from this folder URL instead of GitHub
                   (a mirror; the folder holds the tarball and the sums file)
  -h, --help       this text

Remove it again with uninstall.sh (kept in ~/.local/share/droidtop-agent/).
EOF
}

say() { printf '%s\n' "$*"; }
warn() { printf 'install: %s\n' "$*" >&2; }
die() {
    printf 'install: %s\n' "$*" >&2
    exit 1
}

PREFIX=""
FROM=""
TARBALL=""
RELEASE=""
BASE_URL=""
AUTOSTART=0
DESKTOP=1
while [ $# -gt 0 ]; do
    case $1 in
    --prefix | --from | --tarball | --release | --base-url)
        [ $# -ge 2 ] || die "$1 needs a value"
        case $1 in
        --prefix) PREFIX=$2 ;;
        --from) FROM=$2 ;;
        --tarball) TARBALL=$2 ;;
        --release) RELEASE=$2 ;;
        --base-url) BASE_URL=$2 ;;
        esac
        shift 2
        ;;
    --prefix=*) PREFIX=${1#*=} && shift ;;
    --from=*) FROM=${1#*=} && shift ;;
    --tarball=*) TARBALL=${1#*=} && shift ;;
    --release=*) RELEASE=${1#*=} && shift ;;
    --base-url=*) BASE_URL=${1#*=} && shift ;;
    --autostart) AUTOSTART=1 && shift ;;
    --no-desktop) DESKTOP=0 && shift ;;
    -h | --help) usage && exit 0 ;;
    *) usage >&2 && die "unknown option $1" ;;
    esac
done

[ -n "${HOME:-}" ] || die "HOME is not set"
case $PREFIX in "" | /*) ;; *) die "--prefix must be an absolute path" ;; esac

if [ -n "$PREFIX" ]; then
    BIN_DIR=$PREFIX/bin
    DATA_HOME=$PREFIX/share
else
    [ "$(id -u)" != 0 ] || die "this is a per-user install; as root use the .deb or .rpm, or --prefix /usr/local"
    BIN_DIR=$HOME/.local/bin
    DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
fi
CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}
STATE_DIR=$DATA_HOME/$NAME
MANIFEST=$STATE_DIR/install-manifest

for p in "$BIN_DIR" "$DATA_HOME" "$CONFIG_HOME"; do
    case $p in *"$TAB"* | *"
"*) die "a path with a tab or newline in it cannot be recorded: $p" ;; esac
done

WORK=$(mktemp -d)
cleanup() {
    if [ -n "$WORK" ]; then rm -rf -- "$WORK"; fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM
ENTRIES=$WORK/entries
: >"$ENTRIES"

# ---- where the files come from --------------------------------------------
fetch() { # url dest
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
    else
        die "need curl or wget to download $1"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "need sha256sum or shasum to check the download"
    fi
}

unpack() { # tarball dir
    if command -v zstd >/dev/null 2>&1; then
        zstd -dc "$1" | tar -xf - -C "$2"
    elif tar --zstd -xf "$1" -C "$2" 2>/dev/null; then
        :
    else
        die "need zstd (or a tar that reads .tar.zst) to unpack $1"
    fi
}

arch() {
    case $(uname -m) in
    x86_64 | amd64) echo x86_64 ;;
    aarch64 | arm64) echo aarch64 ;;
    *) die "no build for $(uname -m); there are x86_64 and aarch64" ;;
    esac
}

SRC=
if [ -n "$FROM" ]; then
    SRC=$FROM
elif [ -z "$TARBALL" ] && [ -f "$0" ]; then
    here=$(cd "$(dirname "$0")" && pwd)
    first=${BINS%% *}
    if [ -x "$here/bin/$first" ]; then SRC=$here; fi
fi
if [ -z "$SRC" ]; then
    mkdir "$WORK/x"
    if [ -z "$TARBALL" ]; then
        file=$NAME-linux-$(arch).tar.zst
        if [ -n "$BASE_URL" ]; then
            base=${BASE_URL%/}
        elif [ -n "$RELEASE" ]; then
            base=https://github.com/$REPO/releases/download/$RELEASE
        else
            base=https://github.com/$REPO/releases/latest/download
        fi
        say "Downloading $file from $base"
        fetch "$base/$file" "$WORK/$file"
        fetch "$base/SHA256SUMS" "$WORK/SHA256SUMS"
        want=$(grep " \*\{0,1\}$file\$" "$WORK/SHA256SUMS" | cut -d' ' -f1 | head -n1)
        [ -n "$want" ] || die "$file is not in the release's SHA256SUMS"
        [ "$(sha256_of "$WORK/$file")" = "$want" ] || die "$file does not match its SHA-256 in SHA256SUMS; not installing it"
        say "Checked against SHA256SUMS."
        TARBALL=$WORK/$file
    fi
    unpack "$TARBALL" "$WORK/x"
    SRC=$(find "$WORK/x" -mindepth 1 -maxdepth 1 -type d | head -n1)
    [ -n "$SRC" ] || die "$TARBALL holds no directory"
fi
for b in $BINS; do
    [ -f "$SRC/bin/$b" ] || die "$SRC/bin/$b is missing; not a $NAME package directory"
done
VERSION=unknown
if [ -f "$SRC/VERSION" ]; then VERSION=$(head -n1 "$SRC/VERSION"); fi

# ---- putting files down, and writing them into the manifest ----------------
mk_dir() { # a directory and any missing parents; the ones made are recorded
    d=$1
    missing=
    while [ -n "$d" ] && [ "$d" != / ] && [ ! -d "$d" ]; do
        missing="$d
$missing"
        d=$(dirname "$d")
    done
    [ -n "$missing" ] || return 0
    printf '%s' "$missing" | while IFS= read -r m; do
        [ -n "$m" ] || continue
        mkdir "$m"
        printf 'dir\t%s\n' "$m" >>"$ENTRIES"
    done
}

put() { # source destination mode
    mk_dir "$(dirname "$2")"
    if [ -e "$2" ] || [ -L "$2" ]; then replaced="$replaced  $2
"; fi
    cp "$1" "$2.new.$$"
    chmod "$3" "$2.new.$$"
    mv -f "$2.new.$$" "$2"
    printf 'file\t%s\n' "$2" >>"$ENTRIES"
}

# Reinstalling over an earlier install keeps its entries, so one uninstall
# still removes everything.
if [ -f "$MANIFEST" ]; then grep -E "^(file|dir)$TAB" "$MANIFEST" >>"$ENTRIES" || true; fi

replaced=
for b in $BINS; do put "$SRC/bin/$b" "$BIN_DIR/$b" 755; done
if [ -f "$SRC/uninstall.sh" ]; then put "$SRC/uninstall.sh" "$STATE_DIR/uninstall.sh" 755; fi

if [ "$DESKTOP" = 1 ] && [ -f "$SRC/share/applications/$DESKTOP_ID.desktop" ]; then
    bindir_esc=$(printf '%s' "$BIN_DIR" | sed 's/[\\&|]/\\&/g')
    sed "s|@BINDIR@|$bindir_esc|g" "$SRC/share/applications/$DESKTOP_ID.desktop" >"$WORK/$DESKTOP_ID.desktop"
    put "$WORK/$DESKTOP_ID.desktop" "$DATA_HOME/applications/$DESKTOP_ID.desktop" 644
    if [ -f "$SRC/share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg" ]; then
        put "$SRC/share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg" "$DATA_HOME/icons/hicolor/scalable/apps/$DESKTOP_ID.svg" 644
    fi
fi

# ---- starting at sign-in: the agent's own `autostart on` -------------------
autostart_watch() { # what that command may create, shallowest first
    printf '%s\n' "$CONFIG_HOME" "$CONFIG_HOME/systemd" "$CONFIG_HOME/systemd/user" "$CONFIG_HOME/autostart" \
        "$CONFIG_HOME/systemd/user/$NAME.service" "$CONFIG_HOME/autostart/$NAME.desktop"
}
if [ "$AUTOSTART" = 1 ]; then
    autostart_watch | while IFS= read -r p; do
        if [ -e "$p" ]; then printf '%s\n' "$p"; fi
    done >"$WORK/before"
    if "$BIN_DIR/$NAME" autostart on; then
        autostart_watch | while IFS= read -r p; do
            if [ -e "$p" ] && ! grep -Fxq -- "$p" "$WORK/before"; then
                if [ -d "$p" ]; then printf 'dir\t%s\n' "$p"; else printf 'file\t%s\n' "$p"; fi
            fi
        done >>"$ENTRIES"
    else
        warn "could not turn on starting at sign-in; run \`$NAME autostart on\` yourself"
    fi
fi

# ---- the manifest ----------------------------------------------------------
mk_dir "$STATE_DIR"
{
    printf '# %s install manifest 1\n# version %s\n' "$NAME" "$VERSION"
    sort -u "$ENTRIES"
} >"$WORK/manifest"
cp "$WORK/manifest" "$MANIFEST.new.$$"
mv -f "$MANIFEST.new.$$" "$MANIFEST"

say "Installed $NAME $VERSION for $(id -un):"
for b in $BINS; do say "  $BIN_DIR/$b"; done
[ "$DESKTOP" = 0 ] || say "  menu entry and icon under $DATA_HOME"
[ "$AUTOSTART" = 0 ] || say "  starts when you sign in (turn off: $NAME autostart off)"
if [ -n "$replaced" ]; then
    printf 'Replaced files that were already there:\n%s' "$replaced"
fi
say "  manifest: $MANIFEST"
case ":$PATH:" in
*":$BIN_DIR:"*) ;;
*) say "Note: $BIN_DIR is not on your PATH; add it to run $NAME by name." ;;
esac
say "To remove everything this installed: $STATE_DIR/uninstall.sh   (add --purge to delete your data as well)"
