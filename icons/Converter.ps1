param([string]$Destino = 'C:\Icones\MacTahoe')
# Baixa o tema MacTahoe (github.com/vinceliuice/MacTahoe-icon-theme, GPL-3.0), converte os SVGs
# para .ico com o ImageMagick e monta a pasta que o IconesTahoe.ps1 e o ArcDockWin usam:
#   <Destino>\apps\*.ico      ícones de apps
#   <Destino>\sistema\*.ico   pastas, Lixeira, discos e Este Computador
#   <Destino>\aliases.txt     nomes alternativos (ex.: "1password keyring-manager")
# Requer: ImageMagick 7 (winget install ImageMagick.ImageMagick) e o tar do Windows.

$ErrorActionPreference = 'Stop'
$UseZhTw = [Globalization.CultureInfo]::CurrentUICulture.TwoLetterISOLanguageName -eq 'zh'
function Get-UiText([string]$Text) {
  if (-not $UseZhTw) { return $Text }
  switch ($Text) {
    'ImageMagick não encontrado. Instale com: winget install ImageMagick.ImageMagick (e abra um terminal novo).' { return '找不到 ImageMagick。請安裝：winget install ImageMagick.ImageMagick（然後重新開啟終端機）。' }
    'Baixando o tema MacTahoe...' { return '正在下載 MacTahoe 圖示主題…' }
    'Extraindo...' { return '正在解壓縮…' }
    'Gerando aliases.txt...' { return '正在產生 aliases.txt…' }
    'Convertendo ícones do sistema...' { return '正在轉換系統圖示…' }
    'Convertendo ícones de apps (demora alguns minutos)...' { return '正在轉換應用程式圖示（需數分鐘）…' }
    default { return $Text }
  }
}
if (-not (Get-Command magick -ErrorAction SilentlyContinue)) {
  throw (Get-UiText 'ImageMagick não encontrado. Instale com: winget install ImageMagick.ImageMagick (e abra um terminal novo).')
}

$tmp = Join-Path $env:TEMP 'mactahoe-src'
$tgz = Join-Path $tmp 'tahoe.tar.gz'
New-Item -ItemType Directory -Force $tmp, "$Destino\apps", "$Destino\sistema" | Out-Null

if (-not (Test-Path $tgz)) {
  Write-Host (Get-UiText 'Baixando o tema MacTahoe...')
  Invoke-WebRequest 'https://github.com/vinceliuice/MacTahoe-icon-theme/archive/refs/heads/main.tar.gz' -OutFile $tgz -UseBasicParsing
}

$tar = "$env:SystemRoot\System32\tar.exe"  # o do Windows (o do Git não entende C:\)
Write-Host (Get-UiText 'Extraindo...')
$raiz = 'MacTahoe-icon-theme-main/src'
& $tar -xzf $tgz -C $tmp "$raiz/apps/scalable" "$raiz/places/scalable" "$raiz/devices/scalable" 2>$null
$src = Join-Path $tmp ($raiz -replace '/', '\')

# Os aliases do tema são links simbólicos (que não viram arquivo no Windows): lê a lista do pacote.
Write-Host (Get-UiText 'Gerando aliases.txt...')
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

Write-Host (Get-UiText 'Convertendo ícones do sistema...')
$sis = @(
  'places\folder', 'places\folder-open', 'places\folder-documents', 'places\folder-download',
  'places\folder-images', 'places\folder-music', 'places\folder-videos', 'places\user-desktop',
  'places\user-home', 'places\user-trash', 'places\user-trash-full',
  'devices\computer', 'devices\drive-harddisk', 'devices\drive-harddisk-root', 'devices\drive-optical',
  'devices\drive-removable-media-usb'
) | ForEach-Object { $p = $_ -split '\\'; Join-Path $src "$($p[0])\scalable\$($p[1]).svg" } | Where-Object { Test-Path $_ }
Converter @($sis) "$Destino\sistema"

Write-Host (Get-UiText 'Convertendo ícones de apps (demora alguns minutos)...')
$apps = @(Get-ChildItem "$src\apps\scalable" -Filter *.svg | ForEach-Object FullName)
Converter $apps "$Destino\apps"

Copy-Item (Join-Path $PSScriptRoot 'IconesTahoe.ps1') $Destino -Force
if ($UseZhTw) {
  Write-Host ("完成：{0} 個應用程式圖示、{1} 個系統圖示、{2} 個別名，位置：{3}" -f `
    (Get-ChildItem "$Destino\apps").Count, (Get-ChildItem "$Destino\sistema").Count, $aliases.Count, $Destino)
} else {
  Write-Host ("Pronto: {0} apps, {1} do sistema, {2} aliases em {3}" -f `
    (Get-ChildItem "$Destino\apps").Count, (Get-ChildItem "$Destino\sistema").Count, $aliases.Count, $Destino)
}
