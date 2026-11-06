# Signs one file with the Windows code signing certificate (Authenticode).
# Called automatically by scripts\build.ps1 for the launcher .exe and the installer.
#
# Configure ONE of these environment variables (see README, section "Code signing"):
#   COLDZ_SIGN_PFX            path to a .pfx file  (+ COLDZ_SIGN_PFX_PASSWORD)
#   COLDZ_SIGN_THUMBPRINT     thumbprint of a certificate in the Windows store / USB token
#   COLDZ_SIGN_COMMAND        any custom command, "{file}" is replaced with the file path
#                             (e.g. Azure Artifact Signing or a cloud HSM tool)

param([Parameter(Mandatory = $true)][string]$File)
$ErrorActionPreference = "Stop"

$timestamp = if ($env:COLDZ_SIGN_TIMESTAMP_URL) { $env:COLDZ_SIGN_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }

if ($env:COLDZ_SIGN_COMMAND) {
    $cmd = $env:COLDZ_SIGN_COMMAND.Replace("{file}", $File)
    cmd /c $cmd
    if ($LASTEXITCODE) { throw "Custom sign command failed for $File" }
    exit 0
}

$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe not found (installs with Windows SDK / C++ Build Tools)" }

$signArgs = @("sign", "/fd", "SHA256", "/tr", $timestamp, "/td", "SHA256", "/d", "COLDZ Launcher")
if ($env:COLDZ_SIGN_PFX) {
    $signArgs += @("/f", $env:COLDZ_SIGN_PFX)
    if ($env:COLDZ_SIGN_PFX_PASSWORD) { $signArgs += @("/p", $env:COLDZ_SIGN_PFX_PASSWORD) }
} elseif ($env:COLDZ_SIGN_THUMBPRINT) {
    $signArgs += @("/sha1", $env:COLDZ_SIGN_THUMBPRINT)
} else {
    throw "No certificate configured"
}
$signArgs += $File

& $signtool.FullName @signArgs
if ($LASTEXITCODE) { throw "signtool failed for $File" }
