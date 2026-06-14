[Setup]
AppName=Agentline
AppVersion={#Version}
AppPublisher=seven-tt
AppPublisherURL=https://github.com/seven-tt/agentline
DefaultDirName={autopf}\Agentline
DefaultGroupName=Agentline
OutputDir=.
OutputBaseFilename=agentline-tray-{#Version}-win-x64-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

[Files]
Source: "{#BinDir}\agentline.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\agentline-tray.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Agentline Tray"; Filename: "{app}\agentline-tray.exe"
Name: "{group}\Uninstall Agentline"; Filename: "{uninstallexe}"
Name: "{userstartup}\Agentline Tray"; Filename: "{app}\agentline-tray.exe"; Tasks: autostart

[Tasks]
Name: "autostart"; Description: "Start Agentline Tray at login"; GroupDescription: "Additional options:"
Name: "addtopath"; Description: "Add to PATH"; GroupDescription: "Additional options:"; Flags: checkedonce

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath('{app}')

[Run]
Filename: "{app}\agentline-tray.exe"; Description: "Launch Agentline Tray"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', OrigPath)
  then begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Param + ';', ';' + OrigPath + ';') = 0;
end;
