# Builds the COLDZ launcher installer in one command:  npm run build
# Release build (requires the update signing key):    npm run release -- -Notes "What changed"
#
# Result: release\<version>\COLDZ-Launcher_<version>_x64-setup.exe (+ .sig and latest.json)

param(
    [switch]$Release,
    [string]$Notes = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# --- tools on PATH
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) { $env:PATH = "$cargoBin;$env:PATH" }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw "Rust (cargo) not found. See README (section 1)." }
if (-not (Test-Path "node_modules\.bin\tauri.cmd")) { npm install --no-fund --no-audit; if ($LASTEXITCODE) { throw "npm install failed" } }

$conf = Get-Content "src-tauri\tauri.conf.json" -Raw -Encoding UTF8 | ConvertFrom-Json
$version = $conf.version
$owner = "Poopseeque"; $repo = "coldz-launcher"
if ($conf.plugins.updater.endpoints[0] -match "github\.com/([^/]+)/([^/]+)/") { $owner = $Matches[1]; $repo = $Matches[2] }

# --- extra build settings, merged into tauri.conf.json for this build only
$extra = @{ bundle = @{ windows = @{} } }

# 1) Update signing key (minisign). Without it, players cannot receive auto-updates.
$keyFile = if ($env:COLDZ_UPDATER_KEY) { $env:COLDZ_UPDATER_KEY } else { Join-Path $env:USERPROFILE ".coldz-launcher-keys\updater.key" }
if ($env:TAURI_SIGNING_PRIVATE_KEY) {
    Write-Host "Update key: from TAURI_SIGNING_PRIVATE_KEY" -ForegroundColor Green
} elseif (Test-Path $keyFile) {
    $env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content $keyFile -Raw).Trim()
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "" }
    Write-Host "Update key: $keyFile" -ForegroundColor Green
} elseif ($Release) {
    throw "Update signing key not found ($keyFile). A release without it cannot update players."
} else {
    Write-Warning "Update signing key not found - building WITHOUT update files (.sig / latest.json)."
    $extra.bundle.createUpdaterArtifacts = $false
}

# 2) Windows code signing certificate (Authenticode). Optional until a certificate is bought.
$signScript = Join-Path $PSScriptRoot "sign.ps1"
if ($env:COLDZ_SIGN_PFX -or $env:COLDZ_SIGN_THUMBPRINT -or $env:COLDZ_SIGN_COMMAND) {
    $extra.bundle.windows.signCommand = "powershell -NoProfile -ExecutionPolicy Bypass -File `"$signScript`" `"%1`""
    Write-Host "Code signing: ON" -ForegroundColor Green
} else {
    Write-Warning "Code signing: OFF (no certificate configured) - Windows SmartScreen will show 'Unknown publisher'."
}

$extraFile = Join-Path $env:TEMP "coldz-launcher-build.json"
$extra | ConvertTo-Json -Depth 6 | Set-Content -Path $extraFile -Encoding UTF8

# --- build
& "node_modules\.bin\tauri.cmd" build --config $extraFile
if ($LASTEXITCODE) { throw "tauri build failed" }

# --- collect the result
$nsisDir = "src-tauri\target\release\bundle\nsis"
$setup = Get-ChildItem $nsisDir -Filter "*_${version}_x64-setup.exe" | Select-Object -First 1
if (-not $setup) { throw "Installer not found in $nsisDir" }

$outDir = Join-Path $Root "release\$version"
New-Item -ItemType Directory -Force $outDir | Out-Null
$fileName = "COLDZ-Launcher_${version}_x64-setup.exe"
Copy-Item $setup.FullName (Join-Path $outDir $fileName) -Force

$sigPath = "$($setup.FullName).sig"
if (Test-Path $sigPath) {
    Copy-Item $sigPath (Join-Path $outDir "$fileName.sig") -Force
    $latest = [ordered]@{
        version   = $version
        notes     = $Notes
        pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        platforms = [ordered]@{
            "windows-x86_64" = [ordered]@{
                signature = (Get-Content $sigPath -Raw).Trim()
                url       = "https://github.com/$owner/$repo/releases/download/v$version/$fileName"
            }
        }
    }
    $json = $latest | ConvertTo-Json -Depth 6
    [System.IO.File]::WriteAllText((Join-Path $outDir "latest.json"), $json, (New-Object System.Text.UTF8Encoding($false)))
}

$sizeMb = [math]::Round((Get-Item (Join-Path $outDir $fileName)).Length / 1MB, 2)
Write-Host ""
Write-Host "Done: $outDir" -ForegroundColor Green
Write-Host "  $fileName  ($sizeMb MB)"
if (Test-Path (Join-Path $outDir "latest.json")) { Write-Host "  $fileName.sig, latest.json  -> upload all three to GitHub release v$version" }
