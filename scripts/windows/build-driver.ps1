#Requires -Version 5.1
<#
.SYNOPSIS
    Builds the HushCore Virtual Audio driver package and copies it to dist\driver\windows\<Platform>.

.DESCRIPTION
    Needs Visual Studio 2022 with the Windows Driver Kit extension (WDK 10.0.26100). The
    project is built with msbuild; vswhere locates msbuild.exe. The package consists of
    HushCoreAudio.sys, HushCoreAudio.inf and HushCoreAudio.cat.

.PARAMETER Configuration
    Release (default) or Debug.

.PARAMETER Platform
    x64 (default) or ARM64.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows\build-driver.ps1
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$project = Join-Path $repoRoot 'driver\windows\HushCoreAudio.vcxproj'
$buildOutput = Join-Path $repoRoot ('build\driver\windows\{0}\{1}' -f $Platform, $Configuration)
$distDir = Join-Path $repoRoot ('dist\driver\windows\{0}' -f $Platform)
$packageFiles = @('HushCoreAudio.sys', 'HushCoreAudio.inf', 'HushCoreAudio.cat')

if (-not (Test-Path -LiteralPath $project)) {
    throw "Project file not found: $project"
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw 'vswhere.exe not found. Install Visual Studio 2022 with the "Windows Driver Kit" extension.'
}

$msbuild = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild `
    -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
if ([string]::IsNullOrEmpty($msbuild) -or -not (Test-Path -LiteralPath $msbuild)) {
    throw 'msbuild.exe not found through vswhere.'
}

Write-Host "msbuild: $msbuild"
Write-Host "building $Configuration|$Platform ..."
& $msbuild $project "/p:Configuration=$Configuration" "/p:Platform=$Platform" /m /nologo /v:minimal
if ($LASTEXITCODE -ne 0) {
    throw "msbuild failed with exit code $LASTEXITCODE."
}

foreach ($file in $packageFiles) {
    $source = Join-Path $buildOutput $file
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Build output missing: $source"
    }
}

New-Item -ItemType Directory -Force -Path $distDir | Out-Null
$manifestLines = @()
foreach ($file in $packageFiles) {
    $source = Join-Path $buildOutput $file
    Copy-Item -LiteralPath $source -Destination $distDir -Force
    $hash = (Get-FileHash -LiteralPath (Join-Path $distDir $file) -Algorithm SHA256).Hash
    $manifestLines += ('{0}  {1}' -f $hash, $file)
    Write-Host ('  {0}  sha256={1}' -f $file, $hash)
}

# install-driver.ps1 checks every entry before it hands the package to pnputil.
$manifestPath = Join-Path $distDir 'HushCoreAudio.sha256'
[IO.File]::WriteAllLines($manifestPath, [string[]]$manifestLines, [Text.Encoding]::ASCII)
Write-Host "manifest: $manifestPath"

Write-Host "package: $distDir"
exit 0
