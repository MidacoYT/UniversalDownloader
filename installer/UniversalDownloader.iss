#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{5A6B0A31-399C-45B4-A5D6-23BEA319CF31}
AppName=Universal Downloader
AppVersion={#AppVersion}
AppPublisher=MidacoYT
DefaultDirName={localappdata}\Programs\UniversalDownloader
DefaultGroupName=Universal Downloader
UninstallDisplayIcon={app}\universal-downloader.exe
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputDir=..\dist
OutputBaseFilename=UniversalDownloader-Setup
SetupIconFile=..\assets\app-icon.ico
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=yes
RestartApplications=yes
Uninstallable=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\dist\app\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Universal Downloader"; Filename: "{app}\universal-downloader.exe"
Name: "{autodesktop}\Universal Downloader"; Filename: "{app}\universal-downloader.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\universal-downloader.exe"; Description: "Launch Universal Downloader"; Flags: nowait postinstall skipifsilent
