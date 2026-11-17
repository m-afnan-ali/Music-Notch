; Music Notch installer (Inno Setup 6)
;
; Build (after `cargo build --release`):
;   iscc /DAppVersion=1.0.0 /DAppVersionNum=1.0.0.0 installer\music-notch.iss
; Output: target\installer\Music-Notch-Setup-<version>.exe
;
; Installs per user (no admin prompt) into %LOCALAPPDATA%\Programs\Music Notch,
; adds Start menu / optional desktop shortcuts, an Apps & features entry with
; an uninstaller, and optionally starts Music Notch at sign-in.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef AppVersionNum
  #define AppVersionNum "0.0.0.0"
#endif
#define AppName "Music Notch"
#define AppExe "music-notch.exe"
#define AppPublisher "USRNME"
#define AppURL "https://github.com/m-afnan-ali/Music-Notch"
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define ApprovedKey "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"

[Setup]
; Never change AppId: it is how upgrades find the existing install.
AppId={{8F3C2A71-5B4D-4E9A-9C1F-6D2E7A0B3C45}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#AppVersionNum}
VersionInfoDescription={#AppName} Setup
DefaultDirName={localappdata}\Programs\{#AppName}
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\target\installer
OutputBaseFilename=Music-Notch-Setup-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
CloseApplications=no

[Tasks]
Name: "startup"; Description: "Start Music Notch when I sign in"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Startup entry plus the Startup Apps approval flag (Windows skips entries
; without one). Same values the app writes from its own settings toggle.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "MusicNotch"; ValueData: """{app}\{#AppExe}"""; Tasks: startup
Root: HKCU; Subkey: "{#ApprovedKey}"; ValueType: binary; ValueName: "MusicNotch"; ValueData: "02 00 00 00 00 00 00 00 00 00 00 00"; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
procedure StopRunningApp();
var
  Code: Integer;
begin
  { The notch has no regular window to close, so end it directly. }
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(300);
end;

procedure RemoveStartupEntry();
begin
  RegDeleteValue(HKCU, '{#RunKey}', 'MusicNotch');
  RegDeleteValue(HKCU, '{#ApprovedKey}', 'MusicNotch');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopRunningApp();
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  { Unticking the startup option on an upgrade turns startup off. }
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('startup') then
    RemoveStartupEntry();
end;

function InitializeUninstall(): Boolean;
begin
  StopRunningApp();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  SettingsDir: String;
begin
  if CurUninstallStep = usUninstall then
    RemoveStartupEntry();
  if CurUninstallStep = usPostUninstall then
  begin
    SettingsDir := ExpandConstant('{userappdata}\MusicNotch');
    if DirExists(SettingsDir) and not UninstallSilent() then
      if MsgBox('Also remove your Music Notch settings (position, size and options)?',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(SettingsDir, True, True, True);
  end;
end;
