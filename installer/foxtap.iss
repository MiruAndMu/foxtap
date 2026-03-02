; FoxTap Installer — Inno Setup Script
; Builds FoxTap-v1.0-setup.exe
;
; Prerequisites:
;   - Build plugin: cargo xtask bundle foxtap-plugin --release
;   - Build relay:  cargo build --release -p foxtap-relay
;   - Plugin at:    target/bundled/FoxTap.vst3/
;   - Relay at:     target/release/foxtap-relay.exe

[Setup]
AppName=FoxTap
AppVersion=1.0.0
AppPublisher=Miru & Mu
AppPublisherURL=https://github.com/MiruAndMu
DefaultDirName={autopf}\Miru & Mu\FoxTap
DefaultGroupName=FoxTap
OutputDir=..\target\installer
OutputBaseFilename=FoxTap-v1.0-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName=FoxTap
UninstallDisplayIcon={app}\foxtap-relay.exe
WizardStyle=modern
DisableProgramGroupPage=yes
PrivilegesRequired=admin
SetupIconFile=compiler:SetupClassicIcon.ico

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; VST3 plugin bundle — goes to standard VST3 directory
Source: "..\target\bundled\foxtap-plugin.vst3\*"; DestDir: "{commoncf}\VST3\FoxTap.vst3"; Flags: ignoreversion recursesubdirs createallsubdirs

; Relay executable — goes to our install directory
Source: "..\target\release\foxtap-relay.exe"; DestDir: "{app}"; Flags: ignoreversion

; Also place relay next to VST3 for auto-discovery by plugin
Source: "..\target\release\foxtap-relay.exe"; DestDir: "{commoncf}\VST3\FoxTap.vst3\Contents\x86_64-win"; Flags: ignoreversion

[Icons]
; Start Menu shortcut for manual relay launch
Name: "{group}\FoxTap Relay"; Filename: "{app}\foxtap-relay.exe"; Comment: "Run FoxTap Relay manually"
Name: "{group}\Uninstall FoxTap"; Filename: "{uninstallexe}"

[Registry]
; Store install path so plugin can find relay
Root: HKLM; Subkey: "Software\MiruAndMu\FoxTap"; ValueType: string; ValueName: "RelayPath"; ValueData: "{app}\foxtap-relay.exe"; Flags: uninsdeletekey

[Run]
; After install, check for VB-Cable
Filename: "{cmd}"; Parameters: "/c echo Checking for VB-Cable..."; Flags: runhidden; AfterInstall: CheckVBCable

[Code]
procedure CheckVBCable;
var
  ResultCode: Integer;
begin
  // Check if VB-Cable driver service exists in registry
  if not RegKeyExists(HKEY_LOCAL_MACHINE, 'SYSTEM\CurrentControlSet\Services\VBAudioVACMME') then
  begin
    if MsgBox(
      'VB-Cable virtual audio device was not detected.' + #13#10 + #13#10 +
      'FoxTap needs VB-Cable to route audio to Streamlabs.' + #13#10 +
      'It''s free! Would you like to open the download page?',
      mbConfirmation, MB_YESNO) = IDYES
    then
    begin
      ShellExec('open', 'https://vb-audio.com/Cable/', '', '', SW_SHOW, ewNoWait, ResultCode);
    end;
  end;
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Installation complete — all files deployed
  end;
end;
