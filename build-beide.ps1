# Baut beide Fassungen und legt sie nebeneinander in dist\ ab.
#
# Noetig, weil cargo beide auf denselben Pfad schreibt: target\release\vidinsh.exe.
# Ohne diesen Zwischenschritt ueberschreibt jede Fassung die andere, und
# "vidinsh" waere immer das, was zuletzt gebaut wurde.
#
#   dist\vidinsh.exe              1,6 MB   braucht ffmpeg im System
#   dist\vidinsh-standalone.exe    89 MB   bringt alles mit
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
New-Item -ItemType Directory -Force dist | Out-Null

function Groesse($p) { "{0,6:N1} MB" -f ((Get-Item $p).Length / 1MB) }

Write-Host "[1/2] schlanke Fassung ..." -NoNewline
cargo build --release --quiet
Copy-Item target\release\vidinsh.exe dist\vidinsh.exe -Force
Write-Host " $(Groesse 'dist\vidinsh.exe')"

Write-Host "[2/2] mitgelieferte Fassung (packt ffmpeg, dauert) ..." -NoNewline
cargo build --release --features bundled --quiet
Copy-Item target\release\vidinsh.exe dist\vidinsh-standalone.exe -Force
Write-Host " $(Groesse 'dist\vidinsh-standalone.exe')"

# yt-dlp neben die schlanke Fassung legen. Gesucht wird "tools\" *neben der
# Programmdatei* -- ohne diese Kopie findet dist\vidinsh.exe es nur, wenn man
# zufaellig im Projektordner steht. Die grosse Fassung bringt es selbst mit.
if (Test-Path tools\yt-dlp.exe) {
  New-Item -ItemType Directory -Force dist\tools | Out-Null
  Copy-Item tools\yt-dlp.exe dist\tools\yt-dlp.exe -Force
  Write-Host "      yt-dlp daneben gelegt ($(Groesse 'dist\tools\yt-dlp.exe'))"
}

Write-Host ""
Write-Host "Fertig:"
Get-ChildItem dist\*.exe | ForEach-Object {
  "  {0,-28} {1,9:N1} MB" -f $_.Name, ($_.Length / 1MB)
}
Write-Host ""
Write-Host "  vidinsh.exe             fuer diesen Rechner (nutzt ffmpeg aus dem PATH)"
Write-Host "  vidinsh-standalone.exe  zum Weitergeben (braucht nichts installiertes)"
