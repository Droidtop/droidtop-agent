droidtop-agent for Linux
========================

A small program for this computer that keeps it in step with droidtop on a
handheld: saves, the game library, plugin data. It does not stream or control
the computer (that is windowcast), and it never needs a VPN.

In this package
  bin/droidtop-agent       the command line and the background service
  bin/droidtop-agent-app   the window and tray icon
  install.sh               per-user install, no root
  uninstall.sh             removes what install.sh put down
  LICENSE                  GPL-3.0-only

Install (for you only, nothing needs root)
  sh install.sh                  programs in ~/.local/bin, menu entry and icon
  sh install.sh --autostart      ... and start at sign-in
  droidtop-agent-app             open the window; or from the application menu
  droidtop-agent pair 123456     pair from the command line (the code droidtop shows)

Everything install.sh creates is listed in
~/.local/share/droidtop-agent/install-manifest.

Remove it
  ~/.local/share/droidtop-agent/uninstall.sh            keeps your pairings and saves
  ~/.local/share/droidtop-agent/uninstall.sh --purge    deletes those too

Without installing: run bin/droidtop-agent-app straight from this folder.

Needs a 64-bit Linux with glibc @GLIBC@ or newer. The window needs a graphical
session (Wayland or X11); the tray icon appears where the desktop shows
StatusNotifier icons. The agent listens on port 47610 (TCP and UDP) on the
local network and UDP 47611 for WireGuard.

https://github.com/Droidtop/droidtop-agent
