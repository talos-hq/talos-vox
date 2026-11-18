; Voxtype for Windows: per-user Inno Setup installer (M5b).
;
; Built by the `windows-installer` job in .github/workflows/ci.yml:
;   ISCC /DAppVersion=1.2.3 /DBinDir=<dir with the exes> /DOutputDir=<dir>
;        packaging/windows/voxtype.iss
; which writes voxtype-<version>-windows-x64-setup.exe, the name
; scripts/package-manifests.py and the WinGet manifest expect.
;
; - Per user, no admin prompt: installs under %LOCALAPPDATA%\Programs.
; - Adds the install dir to the user PATH, so `voxtype` works in a new
;   terminal (the WinGet manifest declares the `voxtype` command).
; - Start menu entry for the desktop app (voxtype-app), which starts and
;   supervises the daemon.
; - Optional login item: the HKCU Run key, the same mechanism
;   `voxtype setup autostart` uses, pointing at voxtype-app.
; - Models are downloaded at runtime into the user's data dir and never
;   touched by install or uninstall.

#ifndef AppVersion
  #error Pass /DAppVersion=X.Y.Z
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\target\installer"
#endif

#define AppName "Voxtype"
#define AppPublisher "Talos HQ"
#define AppURL "https://github.com/talos-hq/talos-vox"

[Setup]
; Stable across versions: upgrades replace the existing install.
AppId={{6C1F3B3E-6B8E-4E59-9C9A-7D0F2C5A9B21}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Voxtype
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutputDir}
OutputBaseFilename=voxtype-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\voxtype-app.exe
UninstallDisplayName={#AppName}
LicenseFile=..\..\LICENSE
ChangesEnvironment=yes
; Close a running app/daemon before replacing its files.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start Voxtype when I sign in"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\voxtype.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\voxtype-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\voxtype-app.exe"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\voxtype-app.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Voxtype"; ValueData: """{app}\voxtype-app.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\voxtype-app.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stop the app (it stops the daemon it started) and any daemon left over.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /T /IM voxtype-app.exe"; Flags: runhidden; RunOnceId: "StopApp"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /T /IM voxtype.exe"; Flags: runhidden; RunOnceId: "StopDaemon"

[Code]
const
  EnvKey = 'Environment';

{ The user PATH, without the install dir. }
function PathWithout(const Path, Dir: string): string;
var
  Rest, Part: string;
  P: Integer;
begin
  Result := '';
  Rest := Path;
  while Rest <> '' do
  begin
    P := Pos(';', Rest);
    if P = 0 then
    begin
      Part := Rest;
      Rest := '';
    end
    else
    begin
      Part := Copy(Rest, 1, P - 1);
      Rest := Copy(Rest, P + 1, Length(Rest));
    end;
    if (Part <> '') and (CompareText(RemoveBackslashUnlessRoot(Part), RemoveBackslashUnlessRoot(Dir)) <> 0) then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Part;
    end;
  end;
end;

procedure AddToPath(const Dir: string);
var
  Path: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Path) then
    Path := '';
  Path := PathWithout(Path, Dir);
  if Path <> '' then
    Path := Path + ';';
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Path + Dir);
end;

procedure RemoveFromPath(const Dir: string);
var
  Path: string;
begin
  if RegQueryStringValue(HKCU, EnvKey, 'Path', Path) then
  begin
    Path := PathWithout(Path, Dir);
    if Path = '' then
      RegDeleteValue(HKCU, EnvKey, 'Path')
    else
      RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Path);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
