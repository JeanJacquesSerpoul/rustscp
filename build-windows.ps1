# Compilation de RustSCP pour Windows : exécutable autonome et installateur NSIS.
# Prérequis : Rust stable (MSVC) avec les Build Tools de Visual Studio (C++),
# cible wasm32-unknown-unknown, trunk, tauri-cli 2 ; WebView2 (fourni avec Windows 10/11).
#
#   powershell -ExecutionPolicy Bypass -File build-windows.ps1              # .exe + installateur
#   powershell -ExecutionPolicy Bypass -File build-windows.ps1 -NoInstaller # .exe seul
param(
    [switch]$NoInstaller
)
Set-Location $PSScriptRoot

function Require($cmd, $hint) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Write-Host "$cmd manquant : $hint" -ForegroundColor Red
        exit 1
    }
}

Require cargo 'installez Rust depuis https://rustup.rs'
Require trunk 'cargo install trunk'
cargo tauri --version *> $null
if ($LASTEXITCODE -ne 0) {
    Write-Host "tauri-cli manquant : cargo install tauri-cli --version '^2'" -ForegroundColor Red
    exit 1
}
if (-not (rustup target list --installed | Select-String -SimpleMatch 'wasm32-unknown-unknown')) {
    rustup target add wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# aws-lc-sys (cryptographie de russh) assemble avec NASM ; à défaut, objets précompilés.
if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) {
    $env:AWS_LC_SYS_PREBUILT_NASM = '1'
}

# trunk n'accepte que true/false pour NO_COLOR (souvent positionné à 1)
if ($env:NO_COLOR) { $env:NO_COLOR = 'true' }

if ($NoInstaller) {
    cargo tauri build --no-bundle
} else {
    cargo tauri build --bundles nsis
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$release = Join-Path $PSScriptRoot 'target\release'
Write-Host ''
Write-Host "Exécutable : $(Join-Path $release 'rustscp.exe')" -ForegroundColor Green
if (-not $NoInstaller) {
    Get-ChildItem (Join-Path $release 'bundle\nsis\*.exe') | ForEach-Object {
        Write-Host "Installateur : $($_.FullName)" -ForegroundColor Green
    }
}
