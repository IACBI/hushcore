# Signing and notarization

Unsigned virtual-device builds only load on machines set up for development. Releases must be
signed by the publisher on each platform. No keys or certificates belong in this repository; CI
reads them from encrypted secrets.

## Windows: kernel driver

Windows 10 (1607+) and 11 load only drivers signed by Microsoft.

**Development.** The project builds with `SignMode=TestSign`: Visual Studio signs with a test
certificate. To load it, the developer enables test signing on a VM or test PC
(`bcdedit /set testsigning on`, reboot) and trusts the test certificate. Secure Boot must be off for
test signing.

**Release (attestation signing).**

1. Get an **EV code-signing certificate** and register a company account in the
   [Windows Hardware Dev Center](https://partner.microsoft.com/dashboard/hardware) (Partner Center).
2. Build Release x64 and ARM64. Package each into a CAB containing the `.sys`, `.inf` and `.cat`
   (`makecab /f hushcore.ddf`).
3. Sign the CAB with the EV certificate (`signtool sign /fd sha256 /tr <timestamp-url> /td sha256`).
4. Submit the CAB in Partner Center as **attestation signing** (Windows 10/11 client). Microsoft
   returns the package signed with its certificate.
5. Ship the returned files. Attestation-signed drivers install on retail Windows client editions;
   Windows Server and Windows Update distribution need full HLK certification instead.

Before submitting: run InfVerif (`infverif /w /u`), Code Analysis and Static Driver Verifier on the
project, and the driver must pass Driver Verifier on a test machine.

## macOS: Core Audio plug-in

`coreaudiod` loads unsigned or ad-hoc-signed plug-ins on development machines, but Gatekeeper
blocks downloaded ones. For release:

1. In an Apple Developer account, create a **Developer ID Application** and a **Developer ID
   Installer** certificate.
2. Sign the bundle with the hardened runtime and a secure timestamp:
   `codesign --force --options runtime --timestamp --sign "Developer ID Application: <Name> (<TEAMID>)" HushCoreAudio.driver`
3. Wrap it in an installer package that installs into `/Library/Audio/Plug-Ins/HAL` and restarts
   `coreaudiod` in a postinstall script:
   `pkgbuild --root <root> --identifier com.hushcore.audio.driver --scripts <scripts> --sign "Developer ID Installer: <Name> (<TEAMID>)" HushCoreAudio.pkg`
4. Notarize and staple:
   `xcrun notarytool submit HushCoreAudio.pkg --keychain-profile <profile> --wait`, then
   `xcrun stapler staple HushCoreAudio.pkg`.
5. Sign and notarize `hushcore-engine` the same way if it is distributed as a separate binary.

## Linux

The PipeWire and PulseAudio configuration is plain text and needs no signing. Distribution
packages (deb/rpm) are signed by the repository that publishes them.

## The engine binary

On Windows, sign `hushcore-engine.exe` with the same code-signing certificate (Authenticode,
`signtool sign /fd sha256 /tr <timestamp-url> /td sha256`) so SmartScreen does not warn.

## Current release state

The release workflow (`.github/workflows/release.yml`) builds and labels the artifacts; it does not
sign them. The state of each artifact in the current release:

| Artifact | Signing state |
| --- | --- |
| `hushcore-engine-<tag>-windows-x64.zip`, `-macos-universal.zip`, `-linux-x64.zip` | not signed; not notarized |
| `hushcore-driver-<tag>-windows-testsigned.zip` | test-signed; needs test signing on the machine and `-AllowTestSigned`; attestation signing still to do |
| `hushcore-plugin-<tag>-macos-adhoc.zip` | ad-hoc signed; needs Developer ID signing and notarization |
| `hushcore-virtual-mic-<tag>-linux.zip` | plain text files; no signature |

Because the engine binaries are not notarized, macOS may refuse to run a downloaded copy.
