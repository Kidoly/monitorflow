# Smoke test of the MSI package (ADR-0050), run by CI on a throwaway
# Windows runner as an administrator: silent install, service and ACLs,
# upgrade, the Event Log source, uninstall and cleanup. It changes the
# machine; do not run it on a server you care about.
#
#   deploy/windows/smoke-test.ps1 -Msi <package> -UpgradeMsi <newer package>
param(
    [Parameter(Mandatory)] [string]$Msi,
    [Parameter(Mandatory)] [string]$UpgradeMsi
)
$ErrorActionPreference = "Stop"

$Msi = (Resolve-Path $Msi).Path
$UpgradeMsi = (Resolve-Path $UpgradeMsi).Path
$bin = "$env:ProgramFiles\MonitorFlow"
$data = "$env:ProgramData\MonitorFlow"
$service = "MonitorFlowAgent"
$account = "NT SERVICE\MonitorFlowAgent"
$sid = "S-1-5-80-546755567-1596817566-60379001-3258130523-3598288828"
# msiexec logs; CI uploads them when a step fails.
$temp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$logs = Join-Path $temp "monitorflow-msi"
New-Item -ItemType Directory -Force $logs | Out-Null

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
}

# Runs msiexec and waits for it; a log per step under $logs.
function Msiexec([string]$step, [string[]]$arguments) {
    $log = Join-Path $logs "$step.log"
    $process = Start-Process msiexec.exe -Wait -PassThru `
        -ArgumentList (@($arguments) + @("/qn", "/norestart", "/l*v", "`"$log`""))
    Check ($process.ExitCode -eq 0) "msiexec $step exits with 0 (got $($process.ExitCode), see $log)"
    $log
}

function Service-Info {
    Get-CimInstance Win32_Service -Filter "Name='$service'"
}

function Wait-Status([string]$status) {
    for ($i = 0; $i -lt 30; $i++) {
        if ((Get-Service $service).Status -eq $status) { return $true }
        Start-Sleep -Seconds 1
    }
    $false
}

# The DACL as a sorted list of ACEs, and whether it is protected.
function Dacl([string]$path) {
    $sddl = (Get-Acl $path).GetSecurityDescriptorSddlForm("Access")
    $flags = ($sddl -replace '^D:([A-Z]*)\(.*$', '$1')
    $aces = [regex]::Matches($sddl, '\([^)]*\)') | ForEach-Object Value | Sort-Object
    [pscustomobject]@{ Protected = $flags.Contains("P"); Aces = ($aces -join "") }
}

function Expect-Dacl([string]$path, [string]$serviceRights) {
    $want = @("(A;OICI;FA;;;SY)", "(A;OICI;FA;;;BA)", "(A;OICI;$serviceRights;;;$sid)") | Sort-Object
    $got = Dacl $path
    Check $got.Protected "$path does not inherit"
    Check ($got.Aces -eq ($want -join "")) "$path DACL is $($want -join '') (got $($got.Aces))"
}

# A token of the right shape, so the agent keeps retrying instead of
# stopping on a malformed one.
function Random-Part {
    $bytes = [byte[]]::new(32)
    [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    [Convert]::ToBase64String($bytes).TrimEnd("=").Replace("+", "-").Replace("/", "_")
}
$token = "mfe1.$(Random-Part).$(Random-Part)"

Write-Host "== install"
$log = Msiexec "install" @("/i", "`"$Msi`"", "UPSTREAM_URL=https://127.0.0.1:7443", "ENROLLMENT_TOKEN=$token")
Check (-not (Select-String -Path $log -SimpleMatch $token -Quiet)) "the MSI log does not contain the token"
Check (Test-Path "$bin\monitorflow-agent.exe") "the binary is in $bin"
Check (Test-Path "$bin\agent.windows.example.toml") "the example configuration is in $bin"

$info = Service-Info
Check ($null -ne $info) "service $service exists"
Check ($info.StartName -eq $account) "it runs as $account (got $($info.StartName))"
Check ($info.StartMode -eq "Auto") "it starts automatically (got $($info.StartMode))"
$expected = "`"$bin\monitorflow-agent.exe`" --config `"$data\agent.toml`" service"
Check ($info.PathName -eq $expected) "its command line is $expected (got $($info.PathName))"
$failure = (sc.exe qfailure $service) -join "`n"
Check ($failure -match 'RESET_PERIOD \(in seconds\)\s*:\s*86400') "failure count resets after a day"
Check (([regex]::Matches($failure, 'RESTART -- Delay = 60000')).Count -eq 3) "three restarts after 60 s"

Expect-Dacl $data "0x1200a9"
Expect-Dacl "$data\agent" "0x1301bf"
Expect-Dacl "$data\scripts.d" "0x1200a9"
$config = Get-Content -Raw "$data\agent.toml"
Check ($config.Contains('upstream_url = "https://127.0.0.1:7443"')) "agent.toml has the upstream"
Check ($config.Contains($token)) "agent.toml has the token"
Check (-not (Dacl "$data\agent.toml").Protected) "agent.toml inherits the ACL of $data"
$source = "HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\Application\$service"
Check (Test-Path $source) "the Event Log source is registered"

# Nothing listens on 7443: the agent retries enrollment and stays up.
Check (Wait-Status "Running") "the service is running"
$agentLog = "$data\agent\logs\agent.log"
for ($i = 0; $i -lt 30 -and -not (Select-String -Path $agentLog -SimpleMatch "enrollment attempt failed" -Quiet -ErrorAction SilentlyContinue); $i++) {
    Start-Sleep -Seconds 1
}
Get-Content $agentLog -ErrorAction SilentlyContinue | Write-Host
Check (Select-String -Path $agentLog -SimpleMatch "enrollment attempt failed" -Quiet) "the service read agent.toml and writes its log in $data\agent"

Write-Host "== upgrade"
$configHash = (Get-FileHash "$data\agent.toml").Hash
Set-Content "$data\agent\marker" "identity stand-in"
Msiexec "upgrade" @("/i", "`"$UpgradeMsi`"") | Out-Null
$products = @(Get-ChildItem HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall |
    Get-ItemProperty | Where-Object DisplayName -eq "MonitorFlow agent")
Check ($products.Count -eq 1) "one MonitorFlow agent product after the upgrade (got $($products.Count))"
Check ((Get-FileHash "$data\agent.toml").Hash -eq $configHash) "agent.toml is unchanged"
Check (Test-Path "$data\agent\marker") "the agent's data is kept"
Check ((Service-Info).StartName -eq $account) "the service still runs as $account"
Check (Wait-Status "Running") "the service is running again"

Write-Host "== event log"
Stop-Service $service
Set-Content "$data\agent.toml" "upstream_url = `"not a url`""
$since = (Get-Date).AddSeconds(-5)
# Not Start-Service, which fails when the service stops right away.
sc.exe start $service | Out-Null
Check (Wait-Status "Stopped") "the service stops on an invalid configuration"
$events = @(Get-WinEvent -FilterHashtable @{ LogName = "Application"; ProviderName = $service; StartTime = $since } -ErrorAction SilentlyContinue)
$events | ForEach-Object { Write-Host $_.Message }
Check ($events.Count -ge 1 -and $events[0].Message -match "upstream_url") "the Application log has the reason"

Write-Host "== uninstall, keeping data"
Msiexec "uninstall" @("/x", "`"$UpgradeMsi`"") | Out-Null
Check ($null -eq (Service-Info)) "the service is removed"
Check (-not (Test-Path $bin)) "$bin is removed"
Check (-not (Test-Path $source)) "the Event Log source is removed"
Check (Test-Path "$data\agent.toml") "agent.toml is kept"
Check (Test-Path "$data\agent\marker") "the agent's data is kept"

Write-Host "== reinstall without properties, uninstall with REMOVE_DATA=1"
Msiexec "reinstall" @("/i", "`"$UpgradeMsi`"") | Out-Null
Check ((Get-Service $service).Status -eq "Stopped") "without UPSTREAM_URL the service is not started"
Msiexec "purge" @("/x", "`"$UpgradeMsi`"", "REMOVE_DATA=1") | Out-Null
Check ($null -eq (Service-Info)) "the service is removed"
Check (-not (Test-Path $data)) "$data is removed"
Write-Host "MSI smoke test passed"
