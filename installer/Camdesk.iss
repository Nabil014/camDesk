#ifndef BuildDir
  #define BuildDir "..\desktop\target\release"
#endif
#ifndef FFmpegDir
  #define FFmpegDir "..\desktop\third_party\ffmpeg-n8.1-latest-win64-lgpl-shared-8.1"
#endif

[Setup]
AppId=Camdesk
AppName=Camdesk
AppVersion=0.1.0
DefaultDirName={localappdata}\Programs\Camdesk
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
MinVersion=10.0.22000
OutputDir=..\desktop\target\installer
OutputBaseFilename=Camdesk-Setup
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
Name: "{userprograms}\Camdesk"; Filename: "{app}\camdesk.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\Camdesk"; Filename: "{app}\camdesk.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\camdesk.exe"; Description: "Abrir Camdesk"; Flags: nowait postinstall skipifsilent
