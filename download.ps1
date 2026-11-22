# Morpheus CLI Install Script for Windows PowerShell
#
# Downloads the latest Morpheus CLI binary from GitHub releases and installs it.
#
# Supported OS: Windows
# Supported Architectures: x86_64 (x64)
#
# Usage:
#   iwr -Uri "https://raw.githubusercontent.com/aidreamy/morpheus/main/download.ps1" -OutFile download.ps1; .\download.ps1
#
# Environment variables:
#   $env:MORPHEUS_BIN_DIR  - Installation directory (default: $env:USERPROFILE\morpheus)
#   $env:MORPHEUS_VERSION  - Specific version to install (e.g., "v0.1.0" or "0.1.0")
#   $env:MORPHEUS_VARIANT  - Package variant: "slim" (default, no RAG, no Home) or
#                        "slim-embed" (local RAG via fastembed, no Home).
#                        Voice output ("slim-speak") is not supported on Windows yet.
#
# Release artifacts are produced by .github/workflows/release.yml on tag push
# (v*) and published at github.com/aidreamy/morpheus/releases.

$ErrorActionPreference = "Stop"

$REPO = "aidreamy/morpheus"
$OUT_FILE = "morpheus.exe"

if (-not $env:MORPHEUS_BIN_DIR) {
    $env:MORPHEUS_BIN_DIR = Join-Path $env:USERPROFILE "morpheus"
}

$MORPHEUS_VARIANT = if ($env:MORPHEUS_VARIANT) { $env:MORPHEUS_VARIANT.ToLowerInvariant() } else { "slim" }

# Determine release tag
if ($env:MORPHEUS_VERSION) {
    if ($env:MORPHEUS_VERSION -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+(-.*)?$') {
        Write-Error "Invalid version '$env:MORPHEUS_VERSION'. Expected semver format (e.g. 1.0.0 or v1.0.0)"
        exit 1
    }
    $RELEASE_TAG = if ($env:MORPHEUS_VERSION.StartsWith("v")) { $env:MORPHEUS_VERSION } else { "v$env:MORPHEUS_VERSION" }
} else {
    Write-Host "Fetching latest version..." -ForegroundColor Yellow
    try {
        $api = Invoke-RestMethod -Uri "https://api.github.com/repos/$REPO/releases/latest" -UseBasicParsing
        $RELEASE_TAG = $api.tag_name
    } catch {
        Write-Error "Could not determine latest version. Set MORPHEUS_VERSION to specify one."
        exit 1
    }
}

# Detect architecture
$ARCH = $env:PROCESSOR_ARCHITECTURE
if ($ARCH -eq "AMD64") {
    $ARCH = "x64"
} elseif ($ARCH -eq "ARM64") {
    Write-Error "Windows ARM64 is not currently supported."
    exit 1
} else {
    Write-Error "Unsupported architecture '$ARCH'."
    exit 1
}

# Variant note: both variants are built without the Home screen
# (--no-default-features). "slim-embed" additionally enables the workspace
# `embed` feature (morpheus-tools/embed + fastembed) for local RAG. Both are
# produced by the release workflow.
if ($MORPHEUS_VARIANT -eq "slim-speak") {
    Write-Error "The 'slim-speak' variant (voice output) is not supported on Windows yet. Use 'slim' or 'slim-embed'."
    exit 1
}
if ($MORPHEUS_VARIANT -ne "slim" -and $MORPHEUS_VARIANT -ne "slim-embed") {
    Write-Error "Unsupported MORPHEUS_VARIANT '$MORPHEUS_VARIANT'. Expected 'slim' or 'slim-embed'."
    exit 1
}

$VERSION = $RELEASE_TAG.TrimStart("v")
$SUFFIX = "-$MORPHEUS_VARIANT"
$FILE = "morpheus-v${VERSION}-win32-${ARCH}${SUFFIX}.zip"
$DOWNLOAD_URL = "https://github.com/$REPO/releases/download/$RELEASE_TAG/$FILE"

Write-Host "Downloading morpheus $RELEASE_TAG ($MORPHEUS_VARIANT variant)..." -ForegroundColor Green

try {
    Invoke-WebRequest -Uri $DOWNLOAD_URL -OutFile $FILE -UseBasicParsing
} catch {
    Write-Error "Failed to download $DOWNLOAD_URL"
    Write-Host ""
    Write-Host "If you requested a variant, make sure it exists for this release." -ForegroundColor Yellow
    Write-Host "Try setting `$env:MORPHEUS_VARIANT = 'slim' or removing the environment variable." -ForegroundColor Yellow
    exit 1
}

$TMP_DIR = Join-Path $env:TEMP "morpheus_install_$(Get-Random)"
try {
    New-Item -ItemType Directory -Path $TMP_DIR -Force | Out-Null
} catch {
    Write-Error "Could not create temporary directory."
    exit 1
}

Write-Host "Extracting..." -ForegroundColor Green
try {
    Expand-Archive -Path $FILE -DestinationPath $TMP_DIR -Force
} catch {
    Write-Error "Failed to extract $FILE."
    Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue
    exit 1
}
Remove-Item -Path $FILE -Force

$EXTRACT_DIR = $TMP_DIR
if (Test-Path (Join-Path $TMP_DIR "morpheus-package")) {
    $EXTRACT_DIR = Join-Path $TMP_DIR "morpheus-package"
}

$SOURCE = Join-Path $EXTRACT_DIR "morpheus.exe"
if (-not (Test-Path $SOURCE)) {
    Write-Error "morpheus.exe not found in extracted archive"
    Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue
    exit 1
}

if (-not (Test-Path $env:MORPHEUS_BIN_DIR)) {
    New-Item -ItemType Directory -Path $env:MORPHEUS_BIN_DIR -Force | Out-Null
}

$DEST = Join-Path $env:MORPHEUS_BIN_DIR $OUT_FILE
Write-Host "Installing morpheus to $DEST" -ForegroundColor Green

if (Test-Path $DEST) {
    Remove-Item -Path $DEST -Force
}
Move-Item -Path $SOURCE -Destination $DEST -Force

# The standalone uninstaller ships next to the main binary (morpheus::uninstall
# deletes BOTH images from the same directory).
$UNINSTALL_SOURCE = Join-Path $EXTRACT_DIR "morpheus-uninstall.exe"
if (Test-Path $UNINSTALL_SOURCE) {
    $UNINSTALL_DEST = Join-Path $env:MORPHEUS_BIN_DIR "morpheus-uninstall.exe"
    if (Test-Path $UNINSTALL_DEST) {
        Remove-Item -Path $UNINSTALL_DEST -Force
    }
    Move-Item -Path $UNINSTALL_SOURCE -Destination $UNINSTALL_DEST -Force
}

# Copy runtime DLLs if any
$DLL_FILES = Get-ChildItem -Path $EXTRACT_DIR -Filter "*.dll" -ErrorAction SilentlyContinue
foreach ($dll in $DLL_FILES) {
    $DEST_DLL = Join-Path $env:MORPHEUS_BIN_DIR $dll.Name
    if (Test-Path $DEST_DLL) {
        Remove-Item -Path $DEST_DLL -Force
    }
    Move-Item -Path $dll.FullName -Destination $DEST_DLL -Force
}

Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue

# PATH check
$CURRENT_PATH = $env:PATH
if ($CURRENT_PATH -notlike "*$env:MORPHEUS_BIN_DIR*") {
    Write-Host ""
    Write-Host "Warning: morpheus installed, but $env:MORPHEUS_BIN_DIR is not in your PATH." -ForegroundColor Yellow
    Write-Host "To add it permanently (user scope, no admin required):" -ForegroundColor Yellow
    Write-Host "    [Environment]::SetEnvironmentVariable('PATH', [Environment]::GetEnvironmentVariable('PATH','User') + ';$env:MORPHEUS_BIN_DIR', 'User')" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "For this session only:" -ForegroundColor Yellow
    Write-Host "    `$env:PATH += ';$env:MORPHEUS_BIN_DIR'" -ForegroundColor Cyan
    Write-Host ""
}

Write-Host "morpheus $RELEASE_TAG installed successfully at $DEST" -ForegroundColor Green
Write-Host ""
Write-Host "Run it with: morpheus" -ForegroundColor Cyan
