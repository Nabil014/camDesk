; Con /DAppName=OtroNombre se instala aparte, para probar sin pisar la instalacion real.
#ifndef AppName
  #define AppName "Camdesk"
#endif
#ifndef BuildDir
  #define BuildDir "..\desktop\target\release"
#endif
#ifndef FFmpegDir
  #define FFmpegDir "..\desktop\third_party\ffmpeg-n8.1-latest-win64-lgpl-shared-8.1"
#endif

; La version sale de desktop/crates/app/Cargo.toml: la pasa installer/publish.ps1.
#ifndef AppVersion
  #error Falta la version: compilar con installer/publish.ps1
#endif

[Setup]
AppId={#AppName}
AppName={#AppName}
AppVersion={#AppVersion}
; Al actualizar, la app se cierra sola y la vuelve a abrir la entrada de [Run].
RestartApplications=no
DefaultDirName={localappdata}\Programs\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
MinVersion=10.0.22000
OutputDir=..\desktop\target\installer
OutputBaseFilename=Camdesk-Setup
SetupIconFile=..\desktop\crates\app\assets\camdesk.ico
UninstallDisplayIcon={app}\camdesk.exe
WizardStyle=modern

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[Tasks]
Name: "desktopicon"; Description: "Crear un acceso directo en el escritorio"; Flags: unchecked

[Files]
Source: "{#BuildDir}\camdesk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\vcam.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#FFmpegDir}\bin\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#FFmpegDir}\LICENSE.txt"; DestDir: "{app}"; DestName: "FFmpeg-LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\camdesk.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\camdesk.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\camdesk.exe"; Description: "Abrir Camdesk"; Flags: nowait postinstall skipifsilent
; La actualizacion desde la app instala en silencio y pide /RELAUNCH=1.
Filename: "{app}\camdesk.exe"; Flags: nowait; Check: Relaunch

[Code]
function Relaunch: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;
