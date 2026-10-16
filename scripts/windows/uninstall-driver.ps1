#Requires -Version 5.1
<#
.SYNOPSIS
    Removes the HushCore Virtual Audio device, its driver packages, and its endpoint name keys.

.DESCRIPTION
    Must run elevated. Idempotent: when nothing is installed it reports that and exits 0.
      1. Removes every device instance with hardware ID ROOT\HushCoreVirtualAudio (pnputil /remove-device).
      2. Finds every driver package whose original file is HushCoreAudio.inf with
         Get-WindowsDriver -Online -All (locale-independent) and deletes it from the driver
         store (pnputil /delete-driver <published name> /uninstall /force).
      3. Deletes the two MediaCategories keys that name the endpoints. The GUIDs match
         g_RenderEndpointName and g_CaptureEndpointName in driver/windows/src/Miniport.cpp.

.OUTPUTS
    Exit codes: 0 success or nothing to remove; 1 unexpected error; 2 not elevated; 4 removal failed.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$hardwareId = 'ROOT\HushCoreVirtualAudio'
$originalInfName = 'HushCoreAudio.inf'

# Must match g_RenderEndpointName / g_CaptureEndpointName in driver/windows/src/Miniport.cpp
# and the MediaCategories lines in HushCoreAudio.inf.
$endpointNameKeys = @(
    'HKLM:\SYSTEM\CurrentControlSet\Control\MediaCategories\{802BEB5D-5857-4C68-8C96-268A90F9998C}',
    'HKLM:\SYSTEM\CurrentControlSet\Control\MediaCategories\{9A2675BE-EFCE-4CEC-BC09-93CDBC806495}'
)

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object -TypeName Security.Principal.WindowsPrincipal -ArgumentList $identity
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-HushDevices {
    Get-PnpDevice -ErrorAction SilentlyContinue |
        Where-Object { $_.HardwareID -and (@($_.HardwareID) -contains $hardwareId) }
}

# Published names (oemNN.inf) of every driver package whose original file is HushCoreAudio.inf.
# Get-WindowsDriver returns objects, so the result does not depend on the Windows display language.
function Get-HushDriverPackages {
    $published = @()
    foreach ($driver in @(Get-WindowsDriver -Online -All)) {
        if ([string]::IsNullOrEmpty([string]$driver.OriginalFileName)) {
            continue
        }
        $leaf = [IO.Path]::GetFileName([string]$driver.OriginalFileName)
        if ($leaf -ieq $originalInfName) {
            $published += [string]$driver.Driver
        }
    }
    return $published
}

function Get-HushEndpointKeys {
    $present = @()
    foreach ($key in $endpointNameKeys) {
        if (Test-Path -LiteralPath $key) {
            $present += $key
        }
    }
    return $present
}

try {
    if (-not (Test-Elevated)) {
        Write-Host 'This script must run from an elevated PowerShell (Run as administrator).' -ForegroundColor Red
        exit 2
    }

    $removedDevices = 0
    $devices = @(Get-HushDevices)
    foreach ($device in $devices) {
        Write-Host "removing device: $($device.InstanceId)"
        $out = & pnputil.exe /remove-device $device.InstanceId 2>&1
        $code = $LASTEXITCODE
        $out | ForEach-Object { Write-Host "  $_" }
        if ($code -ne 0) {
            Write-Host "pnputil /remove-device failed with exit code $code." -ForegroundColor Red
            exit 4
        }
        $removedDevices++
    }

    $removedPackages = 0
    $packages = @(Get-HushDriverPackages)
    foreach ($package in $packages) {
        Write-Host "deleting driver package: $package"
        $out = & pnputil.exe /delete-driver $package /uninstall /force 2>&1
        $code = $LASTEXITCODE
        $out | ForEach-Object { Write-Host "  $_" }
        if ($code -ne 0) {
            Write-Host "pnputil /delete-driver failed for $package with exit code $code." -ForegroundColor Red
            exit 4
        }
        $removedPackages++
    }

    $removedKeys = 0
    foreach ($key in @(Get-HushEndpointKeys)) {
        Write-Host "deleting endpoint name key: $key"
        Remove-Item -LiteralPath $key -Recurse -Force
        $removedKeys++
    }

    if ($removedDevices -eq 0 -and $removedPackages -eq 0 -and $removedKeys -eq 0) {
        Write-Host 'Nothing to remove: no HushCore device, driver package or endpoint name key is installed.' -ForegroundColor Green
        exit 0
    }

    $leftoverDevices = @(Get-HushDevices).Count
    $leftoverPackages = @(Get-HushDriverPackages).Count
    $leftoverKeys = @(Get-HushEndpointKeys).Count
    Write-Host ''
    Write-Host ("removed {0} device instance(s), {1} driver package(s), {2} endpoint name key(s)." -f `
        $removedDevices, $removedPackages, $removedKeys)
    if ($leftoverDevices -gt 0 -or $leftoverPackages -gt 0 -or $leftoverKeys -gt 0) {
        Write-Host ("warning: {0} device(s), {1} package(s), {2} key(s) still present; a reboot may be required." -f `
            $leftoverDevices, $leftoverPackages, $leftoverKeys) -ForegroundColor Yellow
        exit 4
    }
    Write-Host 'HushCore Virtual Audio is removed.' -ForegroundColor Green
    exit 0
} catch {
    Write-Host "unexpected error: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
