$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'src-tauri\target\release\scan2bim-converter.exe'
if (-not (Test-Path $exe)) { throw "Tauri release exe not found at $exe (did 'tauri build' succeed?)" }

$version = (Get-Content (Join-Path $root 'package.json') -Raw | ConvertFrom-Json).version
$stage = Join-Path $root "dist\Scan2BIM-Converter-$version-Windows-x64"
$zip   = "$stage.zip"

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
if (Test-Path $zip)   { Remove-Item $zip -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

# 1. Main exe
Copy-Item $exe $stage

# 2. Tauri runtime DLL (WebView2Loader.dll) sits next to the exe in target/release after build
$releaseDir = Split-Path $exe -Parent
Get-ChildItem $releaseDir -Filter "*.dll" -ErrorAction SilentlyContinue | ForEach-Object {
    Copy-Item $_.FullName $stage
}

# 3. Native binaries side-by-side
$binSrc = Join-Path $root 'binaries'
$binDst = Join-Path $stage 'binaries'
Copy-Item $binSrc $binDst -Recurse -Force

# 4. README
@"
Scan2BIM Converter $version (Windows x64)

To run: double-click scan2bim-converter.exe.

This is a portable build — no installer needed. Move the whole
'Scan2BIM-Converter-$version-Windows-x64' folder anywhere and the
app keeps working. The bundled binaries/ subfolder holds the
PDAL and PotreeConverter executables the app uses.

Requirements: Microsoft Edge WebView2 Runtime (pre-installed on
Windows 10/11 since 2022, free download otherwise).

(c) Sohail Saifi for Patrick Staeding
"@ | Out-File -FilePath (Join-Path $stage 'README.txt') -Encoding utf8

Write-Host "Assembling ZIP..."
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal

$zipMB = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ""
Write-Host "DONE: $zip ($zipMB MB)"
Write-Host "Test: extract the ZIP, open the folder, double-click scan2bim-converter.exe"
