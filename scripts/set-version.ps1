# Sets the launcher version in all three places:  npm run set-version -- 0.1.1
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = "Stop"
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
$Root = Split-Path -Parent $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Update-File($path, $pattern, $replacement) {
    $full = Join-Path $Root $path
    $text = [System.IO.File]::ReadAllText($full, $utf8)
    $regex = New-Object System.Text.RegularExpressions.Regex($pattern, [System.Text.RegularExpressions.RegexOptions]::Multiline)
    if (-not $regex.IsMatch($text)) { throw "Version line not found in $path" }
    $text = $regex.Replace($text, $replacement, 1)
    [System.IO.File]::WriteAllText($full, $text, $utf8)
    Write-Host "  $path -> $Version"
}

Update-File "src-tauri\tauri.conf.json" '^(\s*"version":\s*")[^"]+(")' "`${1}$Version`${2}"
Update-File "src-tauri\Cargo.toml"      '^(version\s*=\s*")[^"]+(")'      "`${1}$Version`${2}"
Update-File "package.json"              '^(\s*"version":\s*")[^"]+(")'   "`${1}$Version`${2}"
