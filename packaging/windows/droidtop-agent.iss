; The Windows installer (docs/DESIGN.md section 13, Droidtop/tracker#116): per
; user, no administrator, like install.sh on Linux. It installs the agent, the
; window-and-tray app and windowcast's secure-screen helper into the person's
; own Programs folder, adds a Start menu entry, and can turn on starting at
; sign-in with the agent's own `autostart on` (decision 9: off unless chosen).
; Uninstalling stops the running copies and removes the program and its start
; at sign-in; the person's data (pairings, identity, settings, kept saves) stays.
;
; Built in CI by the windows-installer job:
;   iscc /DAppVersion=<version> /DDist=<folder with the release files> droidtop-agent.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Dist
  #define Dist "..\..\dist"
#endif

[Setup]
AppId={{0920DF79-967A-4E7E-BEC8-954ACB5285EB}
AppName=droidtop-agent
AppVersion={#AppVersion}
AppVerName=droidtop-agent {#AppVersion}
AppPublisher=Droidtop
AppPublisherURL=https://github.com/Droidtop/droidtop-agent
AppSupportURL=https://github.com/Droidtop/droidtop-agent
; Per user only: never asks for an administrator, never offers all users.
PrivilegesRequired=lowest
DefaultDirName={autopf}\droidtop-agent
DisableProgramGroupPage=yes
DisableDirPage=auto
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=out
OutputBaseFilename=droidtop-agent-setup-windows-x86_64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=droidtop-agent
UninstallDisplayIcon={app}\droidtop-agent-app.exe
; The running copies are stopped by the code below, so the files can be replaced.
CloseApplications=no
RestartApplications=no

[Tasks]
Name: "autostart"; Description: "Start droidtop-agent when I sign in (in the tray)"; Flags: unchecked

[Files]
Source: "{#Dist}\droidtop-agent-windows-x86_64.exe"; DestDir: "{app}"; DestName: "droidtop-agent.exe"; Flags: ignoreversion
Source: "{#Dist}\droidtop-agent-app-windows-x86_64.exe"; DestDir: "{app}"; DestName: "droidtop-agent-app.exe"; Flags: ignoreversion
; Service mode looks for the helper beside the agent (crates/service/src/secure_screen.rs, HELPER).
Source: "{#Dist}\windowcast-secure-helper-windows-x86_64.exe"; DestDir: "{app}"; DestName: "windowcast-secure-helper.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\droidtop-agent"; Filename: "{app}\droidtop-agent-app.exe"; Comment: "Keeps this computer in step with your handhelds"

[Run]
; The agent's own autostart (the per-user Run key), so the program and the installer agree on what it is.
Filename: "{app}\droidtop-agent.exe"; Parameters: "autostart on"; Flags: runhidden waituntilterminated; Tasks: autostart; StatusMsg: "Turning on starting at sign-in..."
Filename: "{app}\droidtop-agent-app.exe"; Description: "Open droidtop-agent"; Flags: nowait postinstall skipifsilent

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  RunValue = 'droidtop-agent';

{ Stops every running copy of the two programs, so their files can be replaced or removed. }
procedure StopPrograms();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM droidtop-agent-app.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM droidtop-agent.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopPrograms();
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    StopPrograms();
    { What `droidtop-agent autostart on` made. Removed here, not by running the program,
      which would make its data folders just to turn it off (as uninstall.sh does). }
    RegDeleteValue(HKEY_CURRENT_USER, RunKey, RunValue);
  end;
end;
