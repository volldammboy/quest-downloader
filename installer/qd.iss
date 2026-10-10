; Instalador de Quest Downloader (QD) — Inno Setup 6.
; Ruta por defecto: C:\Program Files\QD

#define MyAppName "Quest Downloader"
#define MyAppExe "QD.exe"
#define MyAppVersion "1.0.1"

[Setup]
AppName={#MyAppName}
AppVersion={#MyAppVersion}
DefaultDirName=C:\Program Files\QD
DisableDirPage=no
DefaultGroupName=Quest Downloader
OutputBaseFilename=QD-Setup-1.0.1
Compression=lzma2/max
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExe}
WizardStyle=modern

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\{#MyAppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Quest Downloader"; Filename: "{app}\{#MyAppExe}"
Name: "{group}\Desinstalar Quest Downloader"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Quest Downloader"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,Quest Downloader}"; Flags: nowait postinstall skipifsilent
