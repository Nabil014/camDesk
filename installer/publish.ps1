#Requires -Version 7
<#
.SYNOPSIS
  Compila Camdesk, arma el instalador y firma la actualizacion. Con -Publicar crea el release.

.DESCRIPTION
  La version sale de desktop/crates/app/Cargo.toml. Para una version nueva: subirla ahi, commitear,
  pushear a main y correr este script con -Publicar. Las apps instaladas la ofrecen al abrirse.

  La clave privada no va en el repo. Si se pierde, las apps instaladas no aceptan mas
  actualizaciones y hay que instalar a mano una version con otra clave publica.
#>
param(
    [switch]$Publicar,
    [string]$Notas = "",
    [string]$Clave = "$env:USERPROFILE\.camdesk\release-key.pem"
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program termino con codigo $LASTEXITCODE" }
}

function Find-Program {
    param([string]$Label, [string[]]$Candidates)
    $found = $Candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $found) { throw "falta $Label" }
    $found
}

$match = Select-String -Path "$root\desktop\crates\app\Cargo.toml" -Pattern '^version = "(\d+\.\d+\.\d+)"'
if (-not $match) { throw "no encuentro la version en desktop/crates/app/Cargo.toml" }
$version = $match.Matches[0].Groups[1].Value
$tag = "v$version"

$iscc = Find-Program "Inno Setup" @("$env:ProgramFiles\Inno Setup 7\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe")
$openssl = Find-Program "OpenSSL (viene con Git)" @("$env:ProgramFiles\Git\usr\bin\openssl.exe", "$env:ProgramFiles\Git\mingw64\bin\openssl.exe")
if (-not (Test-Path $Clave)) { throw "no esta la clave de publicacion: $Clave" }

if ($Publicar) {
    Invoke-Checked git @('-C', $root, 'fetch', '--quiet', 'origin')
    if (git -C $root status --porcelain) { throw "hay cambios sin commitear" }
    $head = git -C $root rev-parse HEAD
    if ($head -ne (git -C $root rev-parse origin/main)) { throw "HEAD no es origin/main: commitear y pushear antes" }
    $existing = gh release list --limit 100 --json tagName --jq '.[].tagName'
    if ($LASTEXITCODE -ne 0) { throw "no pude consultar los releases con gh" }
    if ($existing -contains $tag) { throw "$tag ya esta publicado: subir la version" }
}

# La clave tiene que ser la que las apps instaladas conocen.
$out = "$root\desktop\target\installer"
New-Item -ItemType Directory -Force $out | Out-Null
$publicDer = "$out\publica.der"
Invoke-Checked $openssl @('pkey', '-in', $Clave, '-pubout', '-outform', 'DER', '-out', $publicDer)
$publicHex = -join ([IO.File]::ReadAllBytes($publicDer)[-32..-1] | ForEach-Object { $_.ToString('x2') })
$embedded = (Select-String -Path "$root\desktop\crates\app\src\update.rs" -Pattern 'const PUBLIC_KEY: &str = "([0-9a-f]{64})"').Matches[0].Groups[1].Value
if ($publicHex -ne $embedded) { throw "la clave no coincide con PUBLIC_KEY de update.rs: las apps rechazarian la actualizacion" }

# El instalador es publico: las rutas de esta PC no van dentro de los binarios. Se compila en
# su propia carpeta para no invalidar la compilacion de desarrollo.
$dist = "$root\desktop\target\dist"
$previous = @{ CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR; CARGO_ENCODED_RUSTFLAGS = $env:CARGO_ENCODED_RUSTFLAGS; CFLAGS = $env:CFLAGS }
$env:CARGO_TARGET_DIR = $dist
# rustc aplica el ultimo prefijo que coincida: del mas general al mas especifico.
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$env:USERPROFILE=~",
    "--remap-path-prefix=$root=camdesk",
    '-Clink-arg=/PDBALTPATH:%_PDB%'
) -join [char]0x1f
# El C de las dependencias (ring) guarda __FILE__: se recorta igual.
$env:CFLAGS = "/d1trimfile:$env:USERPROFILE\"
Push-Location "$root\desktop"
try { Invoke-Checked cargo @('build', '--release', '-p', 'camdesk', '-p', 'vcam') }
finally {
    Pop-Location
    foreach ($name in $previous.Keys) { [Environment]::SetEnvironmentVariable($name, $previous[$name]) }
}

foreach ($binary in "$dist\release\camdesk.exe", "$dist\release\vcam.dll") {
    $text = [Text.Encoding]::Latin1.GetString([IO.File]::ReadAllBytes($binary))
    foreach ($private in $env:USERPROFILE, $root) {
        if ($text.IndexOf($private, [StringComparison]::OrdinalIgnoreCase) -ge 0) { throw "$binary lleva la ruta local $private" }
    }
}

Invoke-Checked $iscc @('/Qp', "/DAppVersion=$version", "/DBuildDir=$dist\release", "$root\installer\Camdesk.iss")
$setup = "$out\Camdesk-Setup.exe"
$manifest = "$out\camdesk-update.txt"
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText($manifest, "version=$version`nsha256=$hash`n")
Invoke-Checked $openssl @('pkeyutl', '-sign', '-inkey', $Clave, '-rawin', '-in', $manifest, '-out', "$manifest.sig")
Invoke-Checked $openssl @('pkeyutl', '-verify', '-pubin', '-keyform', 'DER', '-inkey', $publicDer, '-rawin', '-in', $manifest, '-sigfile', "$manifest.sig")
Remove-Item $publicDer

"Camdesk $version listo en $out ($([math]::Round((Get-Item $setup).Length / 1MB, 1)) MB)"
if (-not $Publicar) {
    "No se publico nada. Para publicar: pwsh installer/publish.ps1 -Publicar"
    return
}

$license = "Incluye FFmpeg (LGPL), sin modificar. Fuente: https://github.com/FFmpeg/FFmpeg/tree/release/8.1 · Build: https://github.com/BtbN/FFmpeg-Builds"
$body = (@($Notas, $license) | Where-Object { $_ }) -join "`n`n"
Invoke-Checked gh @('release', 'create', $tag, $setup, $manifest, "$manifest.sig", '--title', "Camdesk $version", '--notes', $body, '--target', $head, '--latest')
