param(
    [string]$TargetDir = "target/release",
    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root $TargetDir
$output = Join-Path $root $OutputDir
$stage = Join-Path $output "raylander-windows-x86_64"
$archive = Join-Path $output "raylander-windows-x86_64.zip"

foreach ($executable in @("raylander.exe", "raylander-tray.exe")) {
    $path = Join-Path $target $executable
    if (-not (Test-Path $path)) {
        throw "Missing $path. Run 'cargo build --release --bins' first."
    }
}

New-Item -ItemType Directory -Force -Path $output | Out-Null
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $stage
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item (Join-Path $target "raylander.exe") $stage
Copy-Item (Join-Path $target "raylander-tray.exe") $stage
Copy-Item (Join-Path $root "examples/config.toml") (Join-Path $stage "config.example.toml")
Copy-Item (Join-Path $root "docs/windows.md") (Join-Path $stage "README-Windows.md")

Remove-Item -Force -ErrorAction SilentlyContinue $archive
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive
Write-Host "Created $archive"
