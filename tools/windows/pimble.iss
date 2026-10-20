; The Windows installer (docs/UPDATE_CONTRACT.md, "The install"): a per-user
; Setup that needs no administrator, puts Pimble where the app's own updater
; keeps it current (%LOCALAPPDATA%\Programs\Pimble), with the install marker, a
; Start-menu entry and an uninstaller in Apps & features.
;
; Built by .github/workflows/release.yml with Inno Setup 6:
;   iscc /DAppVersion=X.Y.Z /DStage=<folder with pimble.exe, pimble-mcp.exe,
;        pimble-install.json> /DOutDir=dist tools\windows\pimble.iss

#ifndef AppVersion
  #error AppVersion is required (/DAppVersion=X.Y.Z)
#endif
#ifndef Stage
  #error Stage is required (/DStage=<folder>)
#endif
#ifndef OutDir
  #define OutDir "dist"
#endif

[Setup]
; Keep the AppId for every release: it is how Windows knows a new Setup is an
; upgrade of this app rather than another one.
AppId={{6E0B5D2A-2E61-4C8B-9F3A-6B1E0C7D4A51}
AppName=Pimble
AppVersion={#AppVersion}
AppVerName=Pimble {#AppVersion}
AppPublisher=Pimble
AppPublisherURL=https://pimble.app
AppSupportURL=https://pimble.app
; The folder update.rs's install_dir() names; per user, so no elevation.
DefaultDirName={localappdata}\Programs\Pimble
DisableDirPage=yes
DefaultGroupName=Pimble
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=pimble-{#AppVersion}-windows-x86_64-setup
SetupIconFile=..\..\assets\brand\pimble.ico
UninstallDisplayIcon={app}\pimble.exe
UninstallDisplayName=Pimble
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; A running Pimble holds pimble.exe open; Setup offers to close it.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "Put Pimble on the desktop"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#Stage}\pimble.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\pimble-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
; The marker that makes this folder a managed install: the app updates itself
; from here on ("Restart to update").
Source: "{#Stage}\pimble-install.json"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Pimble"; Filename: "{app}\pimble.exe"
Name: "{userdesktop}\Pimble"; Filename: "{app}\pimble.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\pimble.exe"; Description: "Open Pimble"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; What the updater leaves beside the program: a staged update, a moved-aside
; old binary. Notes and stores live elsewhere and are never touched.
Type: filesandordirs; Name: "{app}\update"
Type: files; Name: "{app}\*.old"
Type: files; Name: "{app}\*.incoming"
