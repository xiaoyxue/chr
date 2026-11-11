# Dot-source to prepare only the current Windows PowerShell session.
[CmdletBinding()]
param()

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) {
    if (($env:Path -split ';') -notcontains $cargoBin) {
        $env:Path = "$cargoBin;$env:Path"
    }
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw 'Install Rust with rustup before preparing the environment.'
}

$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-msvc'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw 'Install Visual Studio Build Tools with Desktop development with C++ and Windows SDK.'
}
$vsPath = & $vswhere -latest -products '*' `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    throw 'Visual Studio C++ x64 build tools were not found.'
}
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
$installer = Split-Path -Parent $vswhere
if (($env:Path -split ';') -notcontains $installer) {
    $env:Path = "$installer;$env:Path"
}
$environment = & $env:ComSpec /d /c "call `"$vcvars`" >nul && set"
if ($LASTEXITCODE -ne 0) {
    throw 'Could not initialize the MSVC environment.'
}
foreach ($line in $environment) {
    if ($line -match '^([^=]+)=(.*)$') {
        Set-Item -Path "Env:$($matches[1])" -Value $matches[2]
    }
}
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    throw 'The MSVC compiler is not available after environment setup.'
}
& rustc --version
if ($LASTEXITCODE -ne 0) {
    throw 'Install the stable MSVC toolchain: rustup toolchain install stable-x86_64-pc-windows-msvc'
}
Write-Host 'rdc environment ready (Windows x64/MSVC; no global configuration changed).'
