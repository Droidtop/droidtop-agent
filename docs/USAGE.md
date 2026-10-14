# Using droidtop-agent

The full usage notes: the window and command line, pairing, working away from home, save locations, and installing on Linux. For the short version see the [README](../README.md).

## Use

Run **droidtop-agent-app**. It sits in the tray and has a window for
everything below: pairing (it shows or takes a code), the paired handhelds,
the games and apps it found, kept saves, plugin data and settings. On macOS
it is `droidtop-agent.app`. It is not signed yet, so the first time you open
it, use Open from its right-click menu.

The command line does the same for headless machines and scripts:

```
droidtop-agent pair 123456      # the code droidtop shows under Settings > Computers > Pair a computer
droidtop-agent pair             # or: this computer shows its address and a code, typed on the handheld
droidtop-agent                  # keep it running; the handheld connects when it needs to
droidtop-agent scan             # what it found on this computer
droidtop-agent saves steam:440  # where a game's saves are here, and the files
droidtop-agent autostart on     # start when you sign in (off unless you turn it on)
droidtop-agent --help
```

The first time the agent runs, Windows may ask whether it may use the
network. It listens on port 47610 (TCP and UDP) on your local network, and
on UDP 47611 for WireGuard from outside it.

Away from home, the two find each other the way Syncthing does: the agent
learns its public address by STUN and announces it to Syncthing's global
discovery servers, the handheld looks it up, and the two punch through both
routers with WireGuard. Only addresses go to those servers; the sync itself
goes through the direct WireGuard tunnel. `droidtop-agent rendezvous` shows
it, and turns it off or points it at other servers. When the routers do not
allow punching, the handheld still reaches the computer when its UDP 47611 is
reachable: forward it on your router and run
`droidtop-agent endpoint set <public ip>:47611`, or let a global IPv6
address through your firewall. The handheld learns these each time the two
meet. When neither works, `droidtop-agent share set <folder>` names a folder
your own sync tool carries to the handheld (pick the same folder under
droidtop's Settings > Computers > Cloud folder).

Save locations come from the [Ludusavi
manifest](https://github.com/mtkennerly/ludusavi-manifest) (data from
PCGamingWiki). The agent downloads it when it first needs it, or when you run
`droidtop-agent manifest update`. Your own Ludusavi custom games are used too,
and `droidtop-agent saves add` covers anything else.

## Install on Linux

Every release carries four ways to install it (x86_64 and aarch64, 64-bit
Linux with glibc 2.39 or newer), each with its SHA-256 in `SHA256SUMS`:

| File | What it is |
|---|---|
| `droidtop-agent-linux-<arch>.tar.zst` | Portable: the programs, a menu entry, the icon, `install.sh` and `uninstall.sh`. |
| `droidtop-agent-linux-<arch>.run` | One file you can run (the window, or any `droidtop-agent` command) or install from. |
| `droidtop-agent_<version>_<amd64\|arm64>.deb` | For Debian, Ubuntu and relatives. |
| `droidtop-agent-<version>.<x86_64\|aarch64>.rpm` | For Fedora, openSUSE and relatives. |

For you only, without root or a package manager:

```
curl -fsSL https://github.com/Droidtop/droidtop-agent/releases/latest/download/install.sh | sh
curl -fsSL https://github.com/Droidtop/droidtop-agent/releases/latest/download/install.sh | sh -s -- --autostart
```

That downloads the tarball for this machine, checks it against `SHA256SUMS`,
puts the programs in `~/.local/bin` and a menu entry and icon under
`~/.local/share`, and, with `--autostart`, runs `droidtop-agent autostart on`.
The same `install.sh` is in the tarball (`sh install.sh`) and inside the
`.run` file (`./droidtop-agent-linux-x86_64.run install`). Everything it
creates is listed in `~/.local/share/droidtop-agent/install-manifest`.

To remove it, run `~/.local/share/droidtop-agent/uninstall.sh`: it stops the
agent, turns off starting at sign-in and deletes exactly what the manifest
lists. Your pairings, identity, settings and kept saves stay unless you add
`--purge`.

The `.deb` and `.rpm` put the programs in `/usr/bin` and ship a systemd user
unit (`droidtop-agent.service`, the background service) without enabling it;
`systemctl --user enable --now droidtop-agent` turns it on, or use
`droidtop-agent autostart on` for the window. Remove them with `apt remove
droidtop-agent` or `dnf remove droidtop-agent`.

