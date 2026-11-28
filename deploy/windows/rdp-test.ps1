# A remote desktop against this machine's own Remote Desktop (ADR-0192),
# run by CI on a throwaway Windows runner as an administrator: turns
# Remote Desktop on with network level authentication, makes a local
# account allowed to sign in through it, then runs the agent's test that
# opens a desktop through the agent's tunnel to 127.0.0.1:3389, signs in
# and waits for the first picture of the screen
# (crates/agent/tests/desktop_rdp.rs). It changes the machine; do not run
# it on a server you care about.
#
#   deploy/windows/rdp-test.ps1
$ErrorActionPreference = "Stop"

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
}

# Remote Desktop on, network level authentication required, as on a
# server's default.
$ts = "HKLM:\System\CurrentControlSet\Control\Terminal Server"
Set-ItemProperty $ts -Name fDenyTSConnections -Value 0
Set-ItemProperty "$ts\WinStations\RDP-Tcp" -Name UserAuthentication -Value 1
Set-Service TermService -StartupType Manual
Start-Service TermService
$listening = $false
foreach ($i in 1..30) {
    if (Get-NetTCPConnection -LocalPort 3389 -State Listen -ErrorAction SilentlyContinue) {
        $listening = $true
        break
    }
    Start-Sleep -Seconds 2
}
Check $listening "Remote Desktop listens on port 3389"

# A local account for the test only, allowed through Remote Desktop and
# nothing more; its password is made here and masked in the log.
$user = "mfrdptest"
$password = "Mf1!" + [guid]::NewGuid().ToString("N")
if ($env:GITHUB_ACTIONS) { Write-Host "::add-mask::$password" }
$secure = ConvertTo-SecureString $password -AsPlainText -Force
if (Get-LocalUser -Name $user -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $user }
New-LocalUser -Name $user -Password $secure -PasswordNeverExpires -AccountNeverExpires | Out-Null
Add-LocalGroupMember -Group "Remote Desktop Users" -Member $user
Check ($null -ne (Get-LocalUser -Name $user)) "the account $user exists"

try {
    $env:MF_RDP_USERNAME = $user
    $env:MF_RDP_PASSWORD = $password
    $env:MF_RDP_DOMAIN = $env:COMPUTERNAME
    cargo test -p monitorflow-agent --test desktop_rdp -- --ignored --nocapture
    Check ($LASTEXITCODE -eq 0) "a remote desktop opens through the agent's tunnel and shows its screen"
} finally {
    Remove-Item Env:MF_RDP_PASSWORD -ErrorAction SilentlyContinue
    Remove-LocalUser -Name $user -ErrorAction SilentlyContinue
}
Write-Host "Remote desktop test passed"
