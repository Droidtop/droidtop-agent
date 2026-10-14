#!/bin/sh
# Entry point of the single-file build (droidtop-agent-linux-<arch>.run).
# uruntime mounts the image (or unpacks it when there is no FUSE) and runs
# this script with RUNDIR set to where it is.
set -eu
PKG=${RUNDIR:?uruntime did not say where the image is}/pkg

usage() {
    cat <<'TXT'
droidtop-agent in one file.

  ./droidtop-agent-linux-<arch>.run                   open the window (droidtop-agent-app)
  ./droidtop-agent-linux-<arch>.run install [opts]    install for you: ~/.local/bin, a menu entry,
                                                      an icon (--autostart starts it at sign-in)
  ./droidtop-agent-linux-<arch>.run uninstall [opts]  remove what install put there (--purge: your data too)
  ./droidtop-agent-linux-<arch>.run app [args]        droidtop-agent-app
  ./droidtop-agent-linux-<arch>.run agent [args]      droidtop-agent (also: any droidtop-agent
                                                      command, e.g. pair 123456, scan, devices)

Run from the file itself nothing is installed; `autostart on` only makes sense
after `install`. If FUSE is missing it unpacks to a temporary folder first
(or set RUNIMAGE_EXTRACT_AND_RUN=1). Options of the runtime: --runtime-help.
TXT
}

case ${1:-} in
help | -h | --help) usage ;;
install)
    shift
    exec sh "$PKG/install.sh" "$@"
    ;;
uninstall)
    shift
    exec sh "$PKG/uninstall.sh" "$@"
    ;;
"") exec "$PKG/bin/droidtop-agent-app" ;;
app)
    shift
    exec "$PKG/bin/droidtop-agent-app" "$@"
    ;;
agent)
    shift
    exec "$PKG/bin/droidtop-agent" "$@"
    ;;
*) exec "$PKG/bin/droidtop-agent" "$@" ;;
esac
