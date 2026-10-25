# Builds the agent's MSI package (ADR-0050) from a release build of
# monitorflow-agent.exe. Needs the .NET SDK; the WiX version is pinned in
# .config/dotnet-tools.json. Prints the path of the package.
#
#   cargo build --release -p monitorflow-agent
#   deploy/windows/build-msi.ps1
#
# -Version overrides the version from Cargo.toml, for upgrade tests.
param(
    [string]$Exe = "target\release\monitorflow-agent.exe",
    [string]$OutDir = "target\msi",
    [string]$Version = ""
)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$root = (Resolve-Path "$PSScriptRoot\..\..").Path
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

if (-not $Version) {
    $metadata = cargo metadata --no-deps --format-version 1 --manifest-path "$root\Cargo.toml" | ConvertFrom-Json
    $Version = ($metadata.packages | Where-Object name -eq "monitorflow-agent").version
}
# MSI versions are numeric (major.minor.build); drop a pre-release or
# build suffix such as -rc.1.
$msiVersion = ($Version -split '[-+]')[0]

Push-Location $PSScriptRoot
try {
    dotnet tool restore | Out-Null
    # Extensions must match the WiX version.
    $wixVersion = (Get-Content .config\dotnet-tools.json | ConvertFrom-Json).tools.wix.version
    dotnet wix extension add --global "WixToolset.Util.wixext/$wixVersion" | Out-Null
    $msi = Join-Path $OutDir "monitorflow-agent-$Version-x64.msi"
    dotnet wix build -arch x64 -ext WixToolset.Util.wixext `
        -d "Version=$msiVersion" -d "AgentExe=$Exe" `
        -d "ExampleConfig=$root\deploy\config\agent.windows.example.toml" `
        -o $msi Package.wxs Helpers.wxs | Out-Host
} finally {
    Pop-Location
}
# A signing step would go here (ADR-0050): signtool sign on the .exe
# before the build and on the .msi after it.
Write-Output $msi
