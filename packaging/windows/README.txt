droidtop-agent for Windows: the installer
=========================================

droidtop-agent-setup-windows-x86_64.exe installs droidtop-agent for you alone.
It needs no administrator and goes into your own Programs folder
(%LOCALAPPDATA%\Programs\droidtop-agent), with a Start menu entry.

- During setup, "Start droidtop-agent when I sign in" is off unless you tick
  it. It is the same switch as Settings in the window and
  `droidtop-agent autostart on`.
- Running a newer installer over it updates it in place; the running agent
  is stopped first and your pairings stay.
- Uninstall it from Settings > Apps (or Add or remove programs). That stops
  the agent and removes the program and its start at sign-in. Your data
  stays: pairings and this computer's identity, settings and kept saves.

Silent install and uninstall, for scripts:

  droidtop-agent-setup-windows-x86_64.exe /VERYSILENT /SUPPRESSMSGBOXES [/TASKS=autostart]
  "%LOCALAPPDATA%\Programs\droidtop-agent\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES

The installer is not signed yet, so Windows SmartScreen may say it does not
know it: choose More info, then Run anyway.

Built by the windows-installer job in .github/workflows/ci.yml from
droidtop-agent.iss with Inno Setup (the version and hash in pins.env).
