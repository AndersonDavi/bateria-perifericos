# Instala bateria-perifericos.exe en %LOCALAPPDATA%\Programs\BateriaPerifericos,
# lo deja iniciando con Windows y lo arranca. Correr después de `cargo build --release`.
#
#   powershell -ExecutionPolicy Bypass -File .\instalar.ps1
#   powershell -ExecutionPolicy Bypass -File .\instalar.ps1 -Respaldo "G:\Mi unidad\BateriaPerifericos"
#
# -Respaldo: carpeta sincronizada (Google Drive, OneDrive...) donde copiar el historial.
param([string]$Respaldo = "")
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot 'target\release\bateria-perifericos.exe'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\BateriaPerifericos'
$exe = Join-Path $dir 'bateria-perifericos.exe'

# Versión actual y la anterior de un solo mouse (mouse-bateria).
Get-Process bateria-perifericos, mouse-bateria -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $src $exe -Force

if ($Respaldo) {
    $data = Join-Path $env:LOCALAPPDATA 'BateriaPerifericos'
    New-Item -ItemType Directory -Force $data | Out-Null
    Set-Content -Encoding utf8 (Join-Path $data 'respaldo.txt') $Respaldo
}

$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Remove-ItemProperty $run -Name 'MouseBateria' -ErrorAction SilentlyContinue
Set-ItemProperty $run -Name 'BateriaPerifericos' -Value "`"$exe`""
Start-Process $exe
Write-Host "Instalado en $exe y arrancado (inicia con Windows)."
