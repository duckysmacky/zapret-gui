; zapret-gui installer (Inno Setup 6). Built by .github/workflows/release.yml:
;   iscc /DAppVersion=1.2.3 installer\zapret-gui.iss

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif

#ifndef ExePath
  #define ExePath "..\target\release\zapret-gui.exe"
#endif

[Setup]
AppId={{6F3C2B1E-8D4A-4E7B-9C51-2A7D0E9B4F13}
AppName=zapret-gui
AppVersion={#AppVersion}
AppPublisher=zapret-gui contributors
AppComments=Неофициальная программа для zapret-discord-youtube. Не является проектом zapret.
DefaultDirName={autopf}\zapret-gui
DefaultGroupName=zapret-gui
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=Output
OutputBaseFilename=zapret-gui-setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
UninstallDisplayName=zapret-gui
CloseApplications=yes

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"

[Dirs]
Name: "{commonappdata}\zapret-gui"

[Icons]
Name: "{group}\zapret-gui"; Filename: "{app}\zapret-gui.exe"
Name: "{group}\Удалить zapret-gui"; Filename: "{uninstallexe}"

[Run]
; Bundles in ProgramData must be admin-only-writable (SECURITY.md invariant 3). SIDs: names are localized.
Filename: "{sys}\icacls.exe"; Parameters: """{commonappdata}\zapret-gui"" /setowner *S-1-5-32-544 /T /C /Q"; Flags: runhidden
Filename: "{sys}\icacls.exe"; Parameters: """{commonappdata}\zapret-gui"" /inheritance:r /grant:r *S-1-5-32-544:(OI)(CI)F *S-1-5-18:(OI)(CI)F *S-1-5-32-545:(OI)(CI)RX /Q"; Flags: runhidden
Filename: "{app}\zapret-gui.exe"; Description: "Запустить zapret-gui"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM zapret-gui.exe"; Flags: runhidden; RunOnceId: "KillApp"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM winws.exe"; Flags: runhidden; RunOnceId: "KillWinws"
Filename: "{sys}\sc.exe"; Parameters: "stop WinDivert"; Flags: runhidden; RunOnceId: "StopWinDivert"
Filename: "{sys}\sc.exe"; Parameters: "delete WinDivert"; Flags: runhidden; RunOnceId: "DeleteWinDivert"
Filename: "{sys}\sc.exe"; Parameters: "stop WinDivert14"; Flags: runhidden; RunOnceId: "StopWinDivert14"
Filename: "{sys}\sc.exe"; Parameters: "delete WinDivert14"; Flags: runhidden; RunOnceId: "DeleteWinDivert14"
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN zapret-gui /F"; Flags: runhidden; RunOnceId: "DeleteTask"

[UninstallDelete]
Type: filesandordirs; Name: "{commonappdata}\zapret-gui"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
    if MsgBox('Удалить также настройки и журналы zapret-gui (папка %APPDATA%\zapret-gui)?',
              mbConfirmation, MB_YESNO) = IDYES then
      DelTree(ExpandConstant('{userappdata}\zapret-gui'), True, True, True);
end;
