param([switch]$Testar, [switch]$Instalar, [switch]$Desinstalar, [switch]$Restaurar)
# Ícones Tahoe: aplica os ícones do MacTahoe em atalhos, pastas, discos, Lixeira e Este Computador.
# Fica na bandeja; reaplica de X em X horas (pega apps novos e atalhos recriados por atualizações).

$Base       = $PSScriptRoot
$Apps       = Join-Path $Base 'apps'
$Sis        = Join-Path $Base 'sistema'
$BackupFile = Join-Path $Base 'backup.json'
$ConfigFile = Join-Path $Base 'config.json'
$TaskName   = 'Icones Tahoe'
$DockPinned = "$env:APPDATA\ArcDockWin\pinned.txt"
$DockIcons  = "$env:APPDATA\ArcDockWin\icons.txt"

# Executáveis do Windows cujo nome não bate com os nomes do tema (que é de Linux)
$Extra = @{
  msedge = 'microsoft-edge'; chrome = 'google-chrome'; code = 'visual-studio-code'
  cmd = 'terminal'; powershell = 'terminal'; powershell_ise = 'terminal'; pwsh = 'terminal'; wt = 'terminal'; windowsterminal = 'terminal'
  wsl = 'terminal'; 'git-bash' = 'terminal'; 'git-cmd' = 'terminal'; 'git-gui' = 'git'
  explorer = 'file-manager'; taskmgr = 'utilities-system-monitor'; perfmon = 'utilities-system-monitor'
  control = 'preferences-system'; pythonw = 'python'; robloxplayerbeta = 'roblox'; robloxstudiobeta = 'roblox'
  notepad = 'accessories-text-editor'; charmap = 'accessories-character-map'
  mstsc = 'remote-desktop'; cleanmgr = 'accessories-system-cleaner'; dfrgui = 'gnome-disks'
  zapfast = 'whatsapp'; osk = 'preferences-desktop-keyboard'; msconfig = 'preferences-system'; msinfo32 = 'preferences-system'
}

function Test-Admin { ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) }

function Get-Index {
  $idx = @{}
  Get-ChildItem $Apps -Filter *.ico | ForEach-Object { $idx[$_.BaseName.ToLower()] = $_.FullName }
  foreach ($l in Get-Content (Join-Path $Base 'aliases.txt')) {
    $a, $t = $l -split ' ', 2
    $f = $idx[$t.ToLower()]
    if ($f -and -not $idx.ContainsKey($a.ToLower())) { $idx[$a.ToLower()] = $f }
  }
  $idx
}

function Find-Icon($idx, $nome, $exe) {
  $e = [IO.Path]::GetFileNameWithoutExtension($exe).ToLower()
  $n = $nome.ToLower()
  foreach ($k in @($Extra[$e], $e, ($n -replace '[\s_]+', '-'), ($n -replace '[\s_-]+', ''))) {
    if ($k -and $idx[$k]) { return $idx[$k] }
  }
}

function Get-Atalhos {
  $dirs = [Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('CommonDesktopDirectory'),
          [Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('CommonPrograms'),
          "$env:APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"
  Get-ChildItem $dirs -Recurse -Filter *.lnk -ErrorAction SilentlyContinue
}

function Read-Backup {
  $b = @{ lnk = @{}; reg = @() }
  if (Test-Path $BackupFile) {
    $j = Get-Content $BackupFile -Raw -Encoding UTF8 | ConvertFrom-Json
    $j.lnk.PSObject.Properties | ForEach-Object { $b.lnk[$_.Name] = $_.Value }
    $b.reg = @($j.reg | Where-Object { $_ })
  }
  $b
}

function Save-Backup($b) { $b | ConvertTo-Json -Depth 4 | Set-Content $BackupFile -Encoding UTF8 }

function Set-Reg($b, $key, $name, $value) {
  # Guarda o valor original só na primeira vez, para o Restaurar voltar ao estado do Windows
  if (-not ($b.reg | Where-Object { $_.Key -eq $key -and $_.Name -eq $name })) {
    $existe = Test-Path $key
    $old = if ($existe) { (Get-ItemProperty $key -ErrorAction SilentlyContinue).$name } else { $null }
    $b.reg += [pscustomobject]@{ Key = $key; Name = $name; KeyExisted = $existe; Old = $old }
  }
  if (-not (Test-Path $key)) { New-Item $key -Force | Out-Null }
  Set-ItemProperty $key -Name $name -Value $value
}

function Update-Explorer {
  if (-not ('Shell32Notify' -as [type])) {
    Add-Type -Name Shell32Notify -MemberDefinition '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int e, int f, IntPtr a, IntPtr b);'
  }
  [Shell32Notify]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)  # SHCNE_ASSOCCHANGED
  Start-Process ie4uinit.exe -ArgumentList '-show' -WindowStyle Hidden -Wait
}

function Invoke-Aplicar([switch]$SoTestar) {
  $idx = Get-Index
  $b = Read-Backup
  $sh = New-Object -ComObject WScript.Shell
  $trocados = 0; $erros = 0; $sem = @(); $dock = @{}

  foreach ($f in Get-Atalhos) {
    $l = $sh.CreateShortcut($f.FullName)
    if ([IO.Path]::GetExtension($l.TargetPath) -ne '.exe') { continue }
    $ico = Find-Icon $idx $f.BaseName $l.TargetPath
    if ($SoTestar) { '{0,-45} {1}' -f $f.BaseName, $(if ($ico) { Split-Path $ico -Leaf } else { '-' }); continue }
    if (-not $ico) { $sem += $f.BaseName; continue }
    $dock[[IO.Path]::GetFileName($l.TargetPath).ToLower()] = $ico
    $novo = "$ico,0"
    if ($l.IconLocation -eq $novo) { continue }
    try {
      if (-not $b.lnk.ContainsKey($f.FullName)) { $b.lnk[$f.FullName] = $l.IconLocation }
      $l.IconLocation = $novo; $l.Save(); $trocados++
    } catch { $erros++ }
  }
  if ($SoTestar) { return }

  $cu = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\CLSID'
  Set-Reg $b "$cu\{20D04FE0-3AEA-1069-A2D8-08002B30309D}\DefaultIcon" '(default)' "$Sis\computer.ico"
  $lix = "$cu\{645FF040-5081-101B-9F08-00AA002F954E}\DefaultIcon"
  Set-Reg $b $lix '(default)' "$Sis\user-trash-full.ico,0"
  Set-Reg $b $lix 'Empty' "$Sis\user-trash.ico,0"
  Set-Reg $b $lix 'Full' "$Sis\user-trash-full.ico,0"
  $si = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Shell Icons'
  Set-Reg $b $si '3' "$Sis\folder.ico,0"
  Set-Reg $b $si '4' "$Sis\folder-open.ico,0"
  foreach ($d in [IO.DriveInfo]::GetDrives()) {
    $ico = switch ($d.DriveType) {
      'Fixed'     { if ($d.Name -eq "$env:SystemDrive\") { 'drive-harddisk-root' } else { 'drive-harddisk' } }
      'Removable' { 'drive-removable-media-usb' }
      'CDRom'     { 'drive-optical' }
    }
    if ($ico) { Set-Reg $b "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\DriveIcons\$($d.Name[0])\DefaultIcon" '(default)' "$Sis\$ico.ico" }
  }

  # ArcDockWin lê o ícone direto do .exe; ele usa este arquivo quando existe
  if (Test-Path $DockPinned) {
    foreach ($p in Get-Content $DockPinned | Where-Object { $_ -match '\.exe$' }) {
      $ico = Find-Icon $idx ([IO.Path]::GetFileNameWithoutExtension($p)) $p
      if ($ico) { $dock[[IO.Path]::GetFileName($p).ToLower()] = $ico }
    }
  }
  $dock['start'] = $idx['view-app-grid']  # botão Iniciar da dock
  $txt = ($dock.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join "`r`n"
  if ((Test-Path (Split-Path $DockIcons)) -and (-not (Test-Path $DockIcons) -or [IO.File]::ReadAllText($DockIcons) -ne $txt)) {
    [IO.File]::WriteAllText($DockIcons, $txt)  # sem BOM: a dock lê o arquivo direto
  }

  Save-Backup $b
  Update-Explorer
  "{0} atalho(s) trocado(s){1}" -f $trocados, $(if ($erros) { ", $erros com erro" } else { '' })
}

function Invoke-Restaurar {
  $b = Read-Backup
  $sh = New-Object -ComObject WScript.Shell
  foreach ($p in $b.lnk.Keys) {
    if (Test-Path -LiteralPath $p) { try { $l = $sh.CreateShortcut($p); $l.IconLocation = $b.lnk[$p]; $l.Save() } catch {} }
  }
  foreach ($r in $b.reg) {
    if (-not $r.KeyExisted) { Remove-Item $r.Key -Recurse -Force -ErrorAction SilentlyContinue }
    elseif ($null -eq $r.Old) { Remove-ItemProperty $r.Key -Name $r.Name -ErrorAction SilentlyContinue }
    else { Set-ItemProperty $r.Key -Name $r.Name -Value $r.Old }
  }
  Remove-Item $BackupFile, $DockIcons -ErrorAction SilentlyContinue
  Update-Explorer
  'Ícones originais do Windows restaurados'
}

if ($Testar) { Invoke-Aplicar -SoTestar; return }

if (-not (Test-Admin)) {
  # Pastas, discos e atalhos do Menu Iniciar de todos os usuários exigem administrador
  $a = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"")
  if ($Instalar) { $a += '-Instalar' } elseif ($Desinstalar) { $a += '-Desinstalar' } elseif ($Restaurar) { $a += '-Restaurar' }
  Start-Process powershell.exe -Verb RunAs -ArgumentList $a
  return
}

if ($Instalar) {
  $acao = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$PSCommandPath`""
  $gat  = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
  $cfg  = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
  Register-ScheduledTask -TaskName $TaskName -Action $acao -Trigger $gat -Settings $cfg -RunLevel Highest -User "$env:USERDOMAIN\$env:USERNAME" -Force | Out-Null
  Start-ScheduledTask -TaskName $TaskName
  return
}
if ($Desinstalar) {
  Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -like '*IconesTahoe.ps1*' -and $_.ProcessId -ne $PID } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
  Invoke-Restaurar | Out-Null
  Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
  return
}
if ($Restaurar) { Invoke-Restaurar; return }

# ---------- Bandeja ----------
$mutex = New-Object Threading.Mutex($false, 'Global\IconesTahoe')
if (-not $mutex.WaitOne(0)) { return }  # já está rodando

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$horas = 6
if (Test-Path $ConfigFile) { $horas = (Get-Content $ConfigFile -Raw | ConvertFrom-Json).horas }

$tray = New-Object Windows.Forms.NotifyIcon
$tray.Icon = New-Object Drawing.Icon (Join-Path $Apps 'preferences-desktop-icons.ico')
$tray.Text = 'Ícones Tahoe'
$tray.Visible = $true
$timer = New-Object Windows.Forms.Timer

function Aplicar-Agora {
  $tray.Text = 'Ícones Tahoe: aplicando...'
  $msg = Invoke-Aplicar
  $tray.Text = 'Ícones Tahoe'
  $tray.ShowBalloonTip(3000, 'Ícones Tahoe', $msg, 'Info')
}

function Set-Intervalo($h) {
  $script:horas = $h
  @{ horas = $h } | ConvertTo-Json | Set-Content $ConfigFile
  $timer.Stop()
  if ($h -gt 0) { $timer.Interval = $h * 3600000; $timer.Start() }
  foreach ($i in $menuIntervalo.DropDownItems) { $i.Checked = ($i.Tag -eq $h) }
}

$menu = New-Object Windows.Forms.ContextMenuStrip
$menu.Items.Add('Aplicar agora', $null, { Aplicar-Agora }) | Out-Null
$menuIntervalo = New-Object Windows.Forms.ToolStripMenuItem 'Aplicar automaticamente'
foreach ($h in 1, 3, 6, 12, 24, 0) {
  $txt = if ($h -eq 0) { 'Desligado' } elseif ($h -eq 1) { 'A cada 1 hora' } else { "A cada $h horas" }
  $it = New-Object Windows.Forms.ToolStripMenuItem $txt
  $it.Tag = $h
  $it.Add_Click({ param($s) Set-Intervalo $s.Tag })
  $menuIntervalo.DropDownItems.Add($it) | Out-Null
}
$menu.Items.Add($menuIntervalo) | Out-Null
$menu.Items.Add('-') | Out-Null
$menu.Items.Add('Restaurar ícones do Windows', $null, {
  $timer.Stop(); foreach ($i in $menuIntervalo.DropDownItems) { $i.Checked = $false }
  $tray.ShowBalloonTip(3000, 'Ícones Tahoe', (Invoke-Restaurar), 'Info')
}) | Out-Null
$menu.Items.Add('Sair', $null, { $timer.Stop(); $tray.Visible = $false; [Windows.Forms.Application]::Exit() }) | Out-Null
$tray.ContextMenuStrip = $menu
$tray.Add_MouseDoubleClick({ Aplicar-Agora })
$timer.Add_Tick({ Aplicar-Agora })

Set-Intervalo $horas
Aplicar-Agora
[Windows.Forms.Application]::Run()
$tray.Dispose()
$mutex.ReleaseMutex()
