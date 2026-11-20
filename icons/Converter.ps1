param([string]$Destino = 'C:\Icones\MacTahoe')
# Baixa o tema MacTahoe (github.com/vinceliuice/MacTahoe-icon-theme, GPL-3.0), converte os SVGs
# para .ico com o ImageMagick e monta a pasta que o IconesTahoe.ps1 e o ArcDockWin usam:
#   <Destino>\apps\*.ico      ícones de apps
#   <Destino>\sistema\*.ico   pastas, Lixeira, discos e Este Computador
#   <Destino>\aliases.txt     nomes alternativos (ex.: "1password keyring-manager")
# Requer: ImageMagick 7 (winget install ImageMagick.ImageMagick) e o tar do Windows.

$ErrorActionPreference = 'Stop'
if (-not (Get-Command magick -ErrorAction SilentlyContinue)) {
  throw 'ImageMagick não encontrado. Instale com: winget install ImageMagick.ImageMagick (e abra um terminal novo).'
}

$tmp = Join-Path $env:TEMP 'mactahoe-src'
$tgz = Join-Path $tmp 'tahoe.tar.gz'
New-Item -ItemType Directory -Force $tmp, "$Destino\apps", "$Destino\sistema" | Out-Null

if (-not (Test-Path $tgz)) {
  Write-Host 'Baixando o tema MacTahoe...'
  Invoke-WebRequest 'https://github.com/vinceliuice/MacTahoe-icon-theme/archive/refs/heads/main.tar.gz' -OutFile $tgz -UseBasicParsing
}

$tar = "$env:SystemRoot\System32\tar.exe"  # o do Windows (o do Git não entende C:\)
Write-Host 'Extraindo...'
$raiz = 'MacTahoe-icon-theme-main/src'
& $tar -xzf $tgz -C $tmp "$raiz/apps/scalable" "$raiz/places/scalable" "$raiz/devices/scalable" 2>$null
$src = Join-Path $tmp ($raiz -replace '/', '\')

# Os aliases do tema são links simbólicos (que não viram arquivo no Windows): lê a lista do pacote.
Write-Host 'Gerando aliases.txt...'
$aliases = & $tar -tvzf $tgz | ForEach-Object {
  if ($_ -match '/links/apps/scalable/(\S+)\.svg -> (\S+)\.svg$') { "$($Matches[1]) $($Matches[2])" }
}
[IO.File]::WriteAllLines("$Destino\aliases.txt", [string[]]$aliases)

# Avisos do magick vão para o stderr: não podem interromper o lote.
$ErrorActionPreference = 'Continue'

function Converter($svgs, $saida) {
  # mogrify converte vários de uma vez num único processo
  $lote = 200
  for ($i = 0; $i -lt $svgs.Count; $i += $lote) {
    $parte = $svgs[$i..([Math]::Min($i + $lote, $svgs.Count) - 1)]
    magick mogrify -path $saida -format ico -background none -density 400 -resize 256x256 `
      -define icon:auto-resize=256,64,48,32,24,16 $parte 2>$null
    Write-Host ("  {0}/{1}" -f [Math]::Min($i + $lote, $svgs.Count), $svgs.Count)
  }
}

Write-Host 'Convertendo ícones do sistema...'
$sis = @(
  'places\folder', 'places\folder-open', 'places\folder-documents', 'places\folder-download',
  'places\folder-images', 'places\folder-music', 'places\folder-videos', 'places\user-desktop',
  'places\user-home', 'places\user-trash', 'places\user-trash-full',
  'devices\computer', 'devices\drive-harddisk', 'devices\drive-harddisk-root', 'devices\drive-optical',
  'devices\drive-removable-media-usb'
) | ForEach-Object { $p = $_ -split '\\'; Join-Path $src "$($p[0])\scalable\$($p[1]).svg" } | Where-Object { Test-Path $_ }
Converter @($sis) "$Destino\sistema"

Write-Host 'Convertendo ícones de apps (demora alguns minutos)...'
$apps = @(Get-ChildItem "$src\apps\scalable" -Filter *.svg | ForEach-Object FullName)
Converter $apps "$Destino\apps"

Copy-Item (Join-Path $PSScriptRoot 'IconesTahoe.ps1') $Destino -Force
Write-Host ("Pronto: {0} apps, {1} do sistema, {2} aliases em {3}" -f `
  (Get-ChildItem "$Destino\apps").Count, (Get-ChildItem "$Destino\sistema").Count, $aliases.Count, $Destino)
