Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repo = if ($env:THRONGLETS_INSTALL_REPO) { $env:THRONGLETS_INSTALL_REPO } else { "Shangri-la-0428/Thronglets" }
$version = if ($env:THRONGLETS_VERSION) { $env:THRONGLETS_VERSION } else { "latest" }
$installDir = if ($env:THRONGLETS_INSTALL_DIR) { $env:THRONGLETS_INSTALL_DIR } else { Join-Path $HOME ".local\bin" }
$asset = "thronglets-mcp-windows-amd64.exe"
$binPath = Join-Path $installDir "thronglets-bin.exe"
$cmdPath = Join-Path $installDir "thronglets.cmd"
$ps1Path = Join-Path $installDir "thronglets.ps1"

if ($version -eq "latest") {
  $url = "https://github.com/$repo/releases/latest/download/$asset"
} else {
  $url = "https://github.com/$repo/releases/download/v$version/$asset"
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $binPath

$ps1Wrapper = @"
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

& "__BIN_PATH__" @args
exit `$LASTEXITCODE
"@

$cmdWrapper = @"
@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0thronglets.ps1" %*
"@

$ps1Wrapper = $ps1Wrapper.Replace("__BIN_PATH__", $binPath.Replace("\", "\\"))
Set-Content -Path $ps1Path -Value $ps1Wrapper -Encoding UTF8
Set-Content -Path $cmdPath -Value $cmdWrapper -Encoding ASCII

Write-Host "Installed thronglets to $cmdPath"
Write-Host "Next: thronglets start"
