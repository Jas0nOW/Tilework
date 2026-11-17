#define MyAppName "Tilework"
#ifndef MyAppVersion
  #error Pass /DMyAppVersion=MAJOR.MINOR.PATCH from Cargo.toml
#endif
#define MyAppPublisher "Tilework contributors"

[Setup]
AppId={{AC539AC6-9BC0-4D2B-9B89-3D739FF1342C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\Tilework
DefaultGroupName=Tilework
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
MinVersion=10.0.19045
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=output
OutputBaseFilename=Tilework-Setup-{#MyAppVersion}-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
AppMutex=Local\TileworkSingleInstance
CloseApplications=no
RestartApplications=no
UninstallDisplayIcon={app}\tilework.exe
UninstallDisplayName=Tilework
Uninstallable=yes
VersionInfoVersion={#MyAppVersion}.0
VersionInfoProductVersion={#MyAppVersion}
VersionInfoProductTextVersion={#MyAppVersion}
VersionInfoProductName=Tilework
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription=Tilework per-user installer

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\tilework.exe"; DestDir: "{app}"; DestName: "tilework.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Tilework"; Filename: "{app}\tilework.exe"
Name: "{autodesktop}\Tilework"; Filename: "{app}\tilework.exe"; Tasks: desktopicon
Name: "{autoprograms}\Uninstall Tilework"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\tilework.exe"; Description: "Launch Tilework"; Flags: postinstall nowait skipifsilent

[Registry]
Root: HKCU; Subkey: "Software\Tilework"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Tilework"; Flags: uninsdeletevalue

[Code]
function InitializeSetup(): Boolean;
begin
  if IsAdmin then begin
    SuppressibleMsgBox('Run Tilework Setup with standard user permissions.', mbError, MB_OK, IDOK);
    Result := False;
    Exit;
  end;
  Result := not (FileExists(ExpandConstant('{localappdata}\Tilework\tilework.exe')) or
                 FileExists(ExpandConstant('{localappdata}\Tilework\tilework.exe.old')));
  if not Result then
    SuppressibleMsgBox('An older Tilework installation was found. Disable and remove the legacy Tilework scheduled task and remove the old program files before continuing. See the migration steps in the Tilework README.',
      mbError, MB_OK, IDOK);
end;

function InitializeUninstall(): Boolean;
begin
  Result := not IsAdmin;
  if not Result then
    MsgBox('Uninstall Tilework from Windows Settings with standard user permissions.', mbError, MB_OK);
end;
