# Configura el AVD como servidor LAN: UDP/TCP 19132 en el host -> emulador.
# Requiere PowerShell elevado (administrador) y un AVD con la app corriendo.
param(
    [string]$Device = "emulator-5554",
    [string]$UdpBridge = "$env:TEMP\opencode\udpbridge\target\x86_64-linux-android\release\udpbridge",
    [switch]$NoPing
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$relay = Join-Path $PSScriptRoot "bedrock_lan_relay.py"
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
$adb = Join-Path $sdk "platform-tools\adb.exe"
$python = (Get-Command python).Source
$taskName = "BedrockDroidLanRelay"

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Ejecuta este script como administrador (necesita abrir el firewall)."
}

# 1.udpbridge dentro del AVD
$running = & $adb -s $Device shell ps -A 2>$null | Select-String "udpbridge"
if (-not $running) {
    if (-not (Test-Path $UdpBridge)) {
        Write-Error "No encuentro udpbridge en $UdpBridge. Pásalo con -UdpBridge <ruta>."
    }
    & $adb -s $Device push $UdpBridge /data/local/tmp/udpbridge | Out-Null
    & $adb -s $Device shell chmod 755 /data/local/tmp/udpbridge
    & $adb -s $Device shell "/data/local/tmp/udpbridge >/dev/null 2>&1 &"
    Start-Sleep -Milliseconds 500
    Write-Host "udpbridge iniciado en el AVD (TCP 19133 -> UDP 19132 interno)."
} else {
    Write-Host "udpbridge ya estaba corriendo en el AVD."
}

# 2. reenvíos TCP de adb (19132 señalización, 19133 datagramas)
& $adb -s $Device forward tcp:19132 tcp:19132 | Out-Null
& $adb -s $Device forward tcp:19133 tcp:19133 | Out-Null
Write-Host "adb forward 19132 y 19133 configurados."

# 3. reglas de firewall
foreach ($proto in @("UDP", "TCP")) {
    $name = "BedrockDroid LAN $proto 19132"
    if (-not (Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $name -Direction Inbound -Protocol $proto `
            -LocalPort 19132 -Action Allow -Profile Any | Out-Null
    }
}
Write-Host "Firewall UDP/TCP 19132 permitido."

# 4. relay persistente (tarea programada al inicio de sesión)
$tr = "`"$python`" `"$relay`" be4"
if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    schtasks /end /tn $taskName 2>$null | Out-Null
    schtasks /delete /tn $taskName /f 2>$null | Out-Null
}
schtasks /create /tn $taskName /tr $tr /sc onlogon /f | Out-Null
schtasks /run /tn $taskName | Out-Null
Start-Sleep -Seconds 2
Write-Host "Relay iniciado ($taskName)."

# 5. verificación y dirección a usar
$lan = Get-NetIPAddress -AddressFamily IPv4 |
    Where-Object { $_.IPAddress -notmatch '^(127|169\.254)' -and $_.InterfaceAlias -match 'Wi-Fi|Ethernet' } |
    Select-Object -ExpandProperty IPAddress -Unique

Write-Host ""
Write-Host "Servidor LAN listo. En el celular: Minecraft Bedrock > Servidores > Agregar servidor"
foreach ($ip in $lan) { Write-Host "  $ip`:19132" }
Write-Host "Requiere: emulador encendido, app con el servidor ACTIVO y adb conectado."

if (-not $NoPing) {
    $ping = "D:\Repoandroid\tools\raknet-ping.ps1"
    if (Test-Path $ping) {
        foreach ($ip in $lan) {
            Write-Host ""
            Write-Host "Ping a $ip`:19132"
            & powershell -NoProfile -ExecutionPolicy Bypass -File $ping -Target $ip -Port 19132
        }
    }
}
