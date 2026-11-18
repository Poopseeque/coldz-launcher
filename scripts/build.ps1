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

    # 0.5.0: the mirror on coldz.vip (README 7е): the same signed installer, latest.json pointing at coldz.vip.
    # The updater asks GitHub first, then https://coldz.vip/launcher/latest.json (tauri.conf.json endpoints).
    $siteDir = Join-Path $outDir "site\launcher"
    New-Item -ItemType Directory -Force $siteDir | Out-Null
    Copy-Item (Join-Path $outDir $fileName) (Join-Path $siteDir $fileName) -Force
    Copy-Item (Join-Path $outDir "$fileName.sig") (Join-Path $siteDir "$fileName.sig") -Force
    $latest.platforms."windows-x86_64".url = "https://coldz.vip/launcher/$fileName"
    $siteJson = $latest | ConvertTo-Json -Depth 6
    [System.IO.File]::WriteAllText((Join-Path $siteDir "latest.json"), $siteJson, (New-Object System.Text.UTF8Encoding($false)))
    foreach ($f in @("news.json", "news.json.sig")) {
        $src = Join-Path $Root "release\site\launcher\$f"
        if (Test-Path $src) { Copy-Item $src (Join-Path $siteDir $f) -Force }
    }
}

# 0.5.0: the portable build, a second file of the release (README 7и): the exe, portable.txt (the marker: settings and
# logs in data\ next to it), a short text. No auto-update: the launcher sends the player to the download page.
$exe = Get-ChildItem "src-tauri\target\release" -Filter "COLDZ Launcher.exe" | Select-Object -First 1
if ($exe) {
    $portDir = Join-Path $env:TEMP "coldz-portable-$version"
    if (Test-Path $portDir) { Remove-Item -Recurse -Force $portDir }
    New-Item -ItemType Directory -Force $portDir | Out-Null
    Copy-Item $exe.FullName (Join-Path $portDir "COLDZ Launcher.exe")
    $mark = @"
COLDZ Launcher $version, portable / портативная версия.

RU: Распакуйте папку куда угодно и запустите «COLDZ Launcher.exe». Настройки и журналы лежат рядом, в папке data.
Сама версия не обновляется: когда выйдет новая, лаунчер предложит скачать её. Нужен Microsoft Edge WebView2 (есть в
Windows 10/11; если окно не открывается: https://developer.microsoft.com/microsoft-edge/webview2/).
EN: Unpack anywhere and run "COLDZ Launcher.exe". Settings and logs stay next to it, in the data folder. This copy
does not update itself: when a new version is out, the launcher offers to download it. Needs Microsoft Edge WebView2.

COLDZ IS NOT APPROVED, SPONSORED, OR ENDORSED BY ROCKSTAR GAMES.
"@
    [System.IO.File]::WriteAllText((Join-Path $portDir "portable.txt"), $mark, (New-Object System.Text.UTF8Encoding($true)))
    $zipName = "COLDZ-Launcher_${version}_x64-portable.zip"
    $zipPath = Join-Path $outDir $zipName
    if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
    Compress-Archive -Path (Join-Path $portDir "*") -DestinationPath $zipPath
    Remove-Item -Recurse -Force $portDir
}

$sizeMb = [math]::Round((Get-Item (Join-Path $outDir $fileName)).Length / 1MB, 2)
Write-Host ""
Write-Host "Done: $outDir" -ForegroundColor Green
Write-Host "  $fileName  ($sizeMb MB)"
if (Test-Path (Join-Path $outDir "latest.json")) { Write-Host "  $fileName.sig, latest.json  -> upload to GitHub release v$version" }
if ($exe) { Write-Host "  $zipName  (portable, also upload to the release)" }
