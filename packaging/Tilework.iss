#define MyAppName "Tilework"
#ifndef MyAppVersion
  #error Pass /DMyAppVersion=MAJOR.MINOR.PATCH from Cargo.toml
#endif
#define MyAppPublisher "Tilework contributors"
#ifndef MyAppExe
  #define MyAppExe "..\target\release\tilework.exe"
#endif

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
Source: "{#MyAppExe}"; DestDir: "{app}"; DestName: "tilework.exe"; Flags: ignoreversion notimestamp

[Icons]
Name: "{autoprograms}\Tilework"; Filename: "{app}\tilework.exe"
Name: "{autodesktop}\Tilework"; Filename: "{app}\tilework.exe"; Tasks: desktopicon
Name: "{autoprograms}\Uninstall Tilework"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\tilework.exe"; Description: "Launch Tilework"; Flags: postinstall nowait skipifsilent
; One-click updates start Setup with /RelaunchTilework=1 (see src/update.rs): the old instance has
; exited, so bring the new version back up after the files are replaced.
Filename: "{app}\tilework.exe"; Flags: nowait; Check: RelaunchRequested

[Registry]
Root: HKCU; Subkey: "Software\Tilework"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Tilework"; Flags: uninsdeletevalue

[Code]
function LegacyTaskNeedsReview(): Boolean;
var
  Service, Root, Tasks, Task, Definition, Actions, Triggers, Action, Trigger: Variant;
  I: Integer;
begin
  Result := False;
  Service := CreateOleObject('Schedule.Service');
  Service.Connect;
  Root := Service.GetFolder('\');
  Tasks := Root.GetTasks(1); { Include hidden tasks the current user can read. }
  for I := 1 to Tasks.Count do begin
    Task := Tasks.Item(I);
    if CompareText(Task.Name, 'Tilework') = 0 then begin
      Definition := Task.Definition;
      Actions := Definition.Actions;
      Triggers := Definition.Triggers;
      if (Definition.Principal.RunLevel = 1) and (Actions.Count = 1) and
         (Triggers.Count = 1) then begin
        Action := Actions.Item(1);
        Trigger := Triggers.Item(1);
        { TASK_ACTION_EXEC = 0, TASK_TRIGGER_LOGON = 9. }
        Result := (IDispatchInvoke(Action, False, 'Type', []) = 0) and
                  (IDispatchInvoke(Trigger, False, 'Type', []) = 9);
      end;
      Exit;
    end;
  end;
end;

function RelaunchRequested(): Boolean;
begin
  Result := ExpandConstant('{param:RelaunchTilework|0}') = '1';
end;

function InitializeSetup(): Boolean;
var
  LegacyFiles, LegacyTask: Boolean;
begin
  Result := False;
  if IsAdmin then begin
    SuppressibleMsgBox('Run Tilework Setup with standard user permissions.', mbError, MB_OK, IDOK);
    Exit;
  end;
  LegacyFiles := FileExists(ExpandConstant('{localappdata}\Tilework\tilework.exe')) or
                 FileExists(ExpandConstant('{localappdata}\Tilework\tilework.exe.old'));
  try
    LegacyTask := LegacyTaskNeedsReview();
  except
    Log('Could not inspect Task Scheduler: ' + GetExceptionMessage);
    SuppressibleMsgBox('Setup could not inspect Task Scheduler. Confirm that the Windows Task Scheduler service is available and try again.',
      mbError, MB_OK, IDOK);
    Exit;
  end;
  if LegacyFiles or LegacyTask then begin
    Log('Legacy Tilework migration required.');
    SuppressibleMsgBox('Old Tilework files or a scheduled task that needs manual review were found. Follow the Tilework README migration steps before continuing. Setup leaves existing tasks untouched.',
      mbError, MB_OK, IDOK);
    Exit;
  end;
  Result := True;
end;

function InitializeUninstall(): Boolean;
begin
  Result := not IsAdmin;
  if not Result then
    MsgBox('Uninstall Tilework from Windows Settings with standard user permissions.', mbError, MB_OK);
end;
