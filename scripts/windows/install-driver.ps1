#Requires -Version 5.1
<#
.SYNOPSIS
    Installs the HushCore Virtual Audio driver package and creates its root-enumerated device.

.DESCRIPTION
    Must run elevated. Steps:
      1. Checks elevation and that the package (HushCoreAudio.inf/.sys/.cat) exists.
      1a. Verifies every file against HushCoreAudio.sha256 (written by build-driver.ps1) and
          checks the Authenticode signature of HushCoreAudio.cat. Mismatch or an unusable
          signature refuses the install before pnputil runs. A test-signed or untrusted
          catalog is refused unless -AllowTestSigned is given.
      2. Warns, without changing anything, when test signing is off.
      3. pnputil /add-driver <inf> /install.
      4. Creates the device ROOT\HushCoreVirtualAudio through SetupAPI if no such device
         exists, otherwise updates the driver on the existing device. Never creates a second one.
      5. Waits for the endpoints "HushCore Sink" and "HushCore Microphone" to appear.

    This script does not change boot or security settings. Test signing is enabled by the
    developer with the commands that the script prints.

.PARAMETER PackagePath
    Folder containing HushCoreAudio.inf, .sys, .cat and HushCoreAudio.sha256. Default:
    dist\driver\windows\<arch> in the repository root.

.PARAMETER AllowTestSigned
    Accept a catalog that is test-signed or whose signer is not trusted on this machine. The
    signer is printed before installation continues. Without this switch such a package is refused.

.OUTPUTS
    Exit codes: 0 success; 1 unexpected error; 2 not elevated; 3 package missing;
    4 pnputil or driver update failed; 5 device creation failed; 6 endpoints not visible yet;
    7 package integrity or catalog signature check failed (nothing was installed).
#>
[CmdletBinding()]
param(
    [string]$PackagePath,

    [switch]$AllowTestSigned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$hardwareId = 'ROOT\HushCoreVirtualAudio'
$deviceDescription = 'HushCore Virtual Audio'
$endpointNames = @('HushCore Sink', 'HushCore Microphone')
$packageFiles = @('HushCoreAudio.inf', 'HushCoreAudio.sys', 'HushCoreAudio.cat')

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object -TypeName Security.Principal.WindowsPrincipal -ArgumentList $identity
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Returns the integrity problems found in the package folder, as strings. An empty result
# means every packaged file matches its entry in HushCoreAudio.sha256.
function Get-PackageProblems {
    param([string]$Folder)

    $problems = New-Object System.Collections.Generic.List[string]
    $manifestPath = Join-Path $Folder 'HushCoreAudio.sha256'
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        $problems.Add("manifest missing: $manifestPath (rebuild the package with build-driver.ps1)")
        return $problems.ToArray()
    }

    $expected = @{}
    foreach ($line in [IO.File]::ReadAllLines($manifestPath)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $match = [regex]::Match($line, '^([0-9A-Fa-f]{64})\s+(\S+)\s*$')
        if (-not $match.Success) {
            $problems.Add("malformed manifest line: $line")
            continue
        }
        $name = $match.Groups[2].Value
        if ($expected.ContainsKey($name)) {
            $problems.Add("duplicate manifest entry: $name")
            continue
        }
        $expected[$name] = $match.Groups[1].Value.ToUpperInvariant()
    }

    foreach ($file in $packageFiles) {
        if (-not $expected.ContainsKey($file)) {
            $problems.Add("manifest has no hash for $file")
            continue
        }
        $actual = (Get-FileHash -LiteralPath (Join-Path $Folder $file) -Algorithm SHA256).Hash
        if ($actual -ne $expected[$file]) {
            $problems.Add("hash mismatch: $file (manifest $($expected[$file]), file $actual)")
        }
    }
    return $problems.ToArray()
}

# Reads the Authenticode state of the catalog. Trusted means Valid and the signer does not
# look like a test certificate.
function Get-CatalogTrust {
    param([string]$CatalogPath)

    $signature = Get-AuthenticodeSignature -LiteralPath $CatalogPath
    $signer = '<none>'
    if ($null -ne $signature.SignerCertificate) {
        $signer = $signature.SignerCertificate.Subject
    }
    $status = [string]$signature.Status
    $looksLikeTest = ($signer -match '(?i)test')
    return [pscustomobject]@{
        Status  = $status
        Signer  = $signer
        Trusted = (($status -eq 'Valid') -and -not $looksLikeTest)
    }
}

function Get-TestSigningState {
    # Read-only query. Returns $true, $false, or $null when the value cannot be parsed
    # (bcdedit labels are localized on some Windows languages).
    $lines = & bcdedit.exe /enum '{current}' 2>$null
    if ($LASTEXITCODE -ne 0) {
        return $null
    }
    foreach ($line in $lines) {
        $match = [regex]::Match([string]$line, '^\s*testsigning\s+(\S+)\s*$', 'IgnoreCase')
        if ($match.Success) {
            return ($match.Groups[1].Value -ieq 'Yes')
        }
    }
    return $null
}

function Get-HushDevices {
    Get-PnpDevice -ErrorAction SilentlyContinue |
        Where-Object { $_.HardwareID -and (@($_.HardwareID) -contains $hardwareId) }
}

function Add-HushSetupApiType {
    if ('HushCore.Install.HushDeviceSetup' -as [type]) {
        return
    }

    $source = @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace HushCore.Install
{
    [StructLayout(LayoutKind.Sequential)]
    public struct SP_DEVINFO_DATA
    {
        public int cbSize;
        public Guid ClassGuid;
        public uint DevInst;
        public IntPtr Reserved;
    }

    public static class HushDeviceSetup
    {
        private const uint DICD_GENERATE_ID = 0x00000001;
        private const uint SPDRP_HARDWAREID = 0x00000001;
        private const uint DIF_REGISTERDEVICE = 0x00000019;
        private const uint INSTALLFLAG_NONE = 0x00000000;
        private static readonly IntPtr INVALID_HANDLE_VALUE = new IntPtr(-1);
        private static readonly Guid MediaClassGuid = new Guid("4d36e96c-e325-11ce-bfc1-08002be10318");

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid ClassGuid, IntPtr hwndParent);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern bool SetupDiCreateDeviceInfoW(IntPtr DeviceInfoSet, string DeviceName,
            ref Guid ClassGuid, string DeviceDescription, IntPtr hwndParent, uint CreationFlags,
            ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData, uint Property, byte[] PropertyBuffer, uint PropertyBufferSize);

        [DllImport("setupapi.dll", SetLastError = true)]
        private static extern bool SetupDiCallClassInstaller(uint InstallFunction, IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true)]
        private static extern bool SetupDiDestroyDeviceInfoList(IntPtr DeviceInfoSet);

        [DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr hwndParent, string HardwareId,
            string FullInfPath, uint InstallFlags, out bool RebootRequired);

        // Creates one root-enumerated device instance and binds the driver from the INF.
        public static bool CreateDevice(string hardwareId, string description, string infPath)
        {
            Guid classGuid = MediaClassGuid;
            IntPtr set = SetupDiCreateDeviceInfoList(ref classGuid, IntPtr.Zero);
            if (set == INVALID_HANDLE_VALUE)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfoList failed");
            }

            try
            {
                SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
                data.cbSize = Marshal.SizeOf(typeof(SP_DEVINFO_DATA));

                if (!SetupDiCreateDeviceInfoW(set, "HushCoreVirtualAudio", ref classGuid, description,
                        IntPtr.Zero, DICD_GENERATE_ID, ref data))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfoW failed");
                }

                // REG_MULTI_SZ: each string is NUL-terminated and the list ends with an extra NUL.
                byte[] hardwareIds = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
                if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, hardwareIds,
                        (uint)hardwareIds.Length))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiSetDeviceRegistryPropertyW failed");
                }

                if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCallClassInstaller failed");
                }
            }
            finally
            {
                SetupDiDestroyDeviceInfoList(set);
            }

            return BindDriver(hardwareId, infPath);
        }

        // Updates the driver on existing device instances. Returns true when a reboot is required.
        public static bool BindDriver(string hardwareId, string infPath)
        {
            bool rebootRequired;
            if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hardwareId, infPath, INSTALLFLAG_NONE,
                    out rebootRequired))
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "UpdateDriverForPlugAndPlayDevicesW failed");
            }
            return rebootRequired;
        }
    }
}
'@

    Add-Type -TypeDefinition $source -Language CSharp
}

# ---- main ---------------------------------------------------------------------------------

try {
    if (-not (Test-Elevated)) {
        Write-Host 'This script must run from an elevated PowerShell (Run as administrator).' -ForegroundColor Red
        exit 2
    }

    if ([string]::IsNullOrEmpty($PackagePath)) {
        $arch = 'x64'
        if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
            $arch = 'ARM64'
        }
        $PackagePath = Join-Path $repoRoot ('dist\driver\windows\' + $arch)
    }
    $resolved = Resolve-Path -LiteralPath $PackagePath -ErrorAction SilentlyContinue
    if ($null -eq $resolved) {
        Write-Host 'Driver package folder not found. Run scripts\windows\build-driver.ps1 first, or pass -PackagePath.' -ForegroundColor Red
        exit 3
    }
    $PackagePath = $resolved.ProviderPath

    foreach ($file in $packageFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $PackagePath $file))) {
            Write-Host "Package incomplete, missing: $file in $PackagePath" -ForegroundColor Red
            exit 3
        }
    }
    $infPath = Join-Path $PackagePath 'HushCoreAudio.inf'
    Write-Host "package: $PackagePath"

    $problems = @(Get-PackageProblems -Folder $PackagePath)
    if ($problems.Count -gt 0) {
        foreach ($problem in $problems) {
            Write-Host "  $problem" -ForegroundColor Red
        }
        Write-Host 'Refusing to install: the package does not match its hash manifest. Rebuild it with build-driver.ps1.' -ForegroundColor Red
        exit 7
    }
    Write-Host 'hash manifest: all packaged files match.'

    $catalog = Get-CatalogTrust -CatalogPath (Join-Path $PackagePath 'HushCoreAudio.cat')
    if ($catalog.Status -in @('NotSigned', 'HashMismatch', 'Incompatible')) {
        Write-Host "Refusing to install: catalog signature is unusable (status $($catalog.Status))." -ForegroundColor Red
        exit 7
    }
    if ($catalog.Trusted) {
        Write-Host "catalog signature: Valid, signer: $($catalog.Signer)"
    } else {
        Write-Host "catalog signature: $($catalog.Status), signer: $($catalog.Signer)" -ForegroundColor Yellow
        if (-not $AllowTestSigned) {
            Write-Host 'Refusing to install: the catalog is test-signed or its signer is not trusted here.' -ForegroundColor Red
            Write-Host 'If you built this package yourself, rerun with -AllowTestSigned.' -ForegroundColor Yellow
            exit 7
        }
        Write-Host 'Continuing because -AllowTestSigned was given.' -ForegroundColor Yellow
    }

    $testSigning = Get-TestSigningState
    if ($testSigning -eq $false) {
        Write-Host ''
        Write-Host 'Test signing is OFF. A test-signed package will not load until it is enabled.' -ForegroundColor Yellow
        Write-Host 'This script does not change it. To enable it, run these yourself and reboot:' -ForegroundColor Yellow
        Write-Host '    bcdedit /set testsigning on'
        Write-Host '    shutdown /r /t 0'
        Write-Host ''
    } elseif ($null -eq $testSigning) {
        Write-Host 'Could not read the testsigning state from bcdedit (localized output). Continuing.' -ForegroundColor Yellow
    }

    Write-Host 'adding the driver package ...'
    $addOutput = & pnputil.exe /add-driver $infPath /install 2>&1
    $addExit = $LASTEXITCODE
    $addOutput | ForEach-Object { Write-Host "  $_" }
    if ($addExit -ne 0 -and $addExit -ne 3010) {
        Write-Host "pnputil /add-driver failed with exit code $addExit." -ForegroundColor Red
        if ($testSigning -eq $false) {
            Write-Host 'Most likely cause: test signing is off (see above).' -ForegroundColor Yellow
        }
        exit 4
    }
    $rebootNeeded = ($addExit -eq 3010)

    Add-HushSetupApiType
    $existing = @(Get-HushDevices)
    if ($existing.Count -eq 0) {
        Write-Host "creating device $hardwareId ..."
        try {
            $rebootNeeded = [HushCore.Install.HushDeviceSetup]::CreateDevice($hardwareId, $deviceDescription, $infPath) -or $rebootNeeded
        } catch {
            Write-Host "device creation failed: $($_.Exception.Message)" -ForegroundColor Red
            exit 5
        }
    } else {
        Write-Host "device already present ($($existing.Count) instance(s)); updating its driver ..."
        try {
            $rebootNeeded = [HushCore.Install.HushDeviceSetup]::BindDriver($hardwareId, $infPath) -or $rebootNeeded
        } catch {
            Write-Host "driver update failed: $($_.Exception.Message)" -ForegroundColor Red
            exit 4
        }
    }

    Write-Host 'waiting for the endpoints ...'
    $missing = @($endpointNames)
    for ($attempt = 0; $attempt -lt 15 -and $missing.Count -gt 0; $attempt++) {
        Start-Sleep -Seconds 1
        $endpoints = @(Get-PnpDevice -Class AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue)
        $missing = @($endpointNames | Where-Object {
            $name = $_
            -not ($endpoints | Where-Object { $_.FriendlyName -like "*$name*" })
        })
    }

    foreach ($name in $endpointNames) {
        if ($missing -contains $name) {
            Write-Host ("  NOT FOUND  {0}" -f $name) -ForegroundColor Yellow
        } else {
            Write-Host ("  found      {0}" -f $name) -ForegroundColor Green
        }
    }

    if ($rebootNeeded) {
        Write-Host 'A reboot is required to finish the installation.' -ForegroundColor Yellow
    }

    if ($missing.Count -gt 0) {
        Write-Host 'Endpoints are not visible yet. Check Sound settings, or reboot and rerun this script.' -ForegroundColor Yellow
        exit 6
    }

    Write-Host 'HushCore Virtual Audio is installed. Verify with: hushcore-engine virtual-device' -ForegroundColor Green
    exit 0
} catch {
    Write-Host "unexpected error: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
