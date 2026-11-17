$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$root = (Resolve-Path "$PSScriptRoot/../..").Path
Push-Location $root
$oldToolchain = $env:RUSTUP_TOOLCHAIN
$oldRustflags = $env:RUSTFLAGS
try {
    $env:RUSTUP_TOOLCHAIN = "1.95.0"
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    rustup target add --toolchain 1.95.0 x86_64-pc-windows-msvc
    cargo build --release --locked --target x86_64-pc-windows-msvc

    # Check imports rather than assuming crt-static removed the VC++ runtime dependency.
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products "*" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) { throw "Visual Studio C++ tools not found" }
    $dumpbin = Resolve-Path "$vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" |
        Sort-Object Path -Descending | Select-Object -First 1 -ExpandProperty Path
    $imports = & $dumpbin /DEPENDENTS "target\x86_64-pc-windows-msvc\release\oxeye.exe"
    $imports | Write-Output
    if ($imports -match '(?i)^\s*(VCRUNTIME\S*|MSVCP\S*|ucrtbase|api-ms-win-crt-\S*)\.dll\s*$') {
        throw "Unexpected dynamic VC++ runtime dependency"
    }

    New-Item -ItemType Directory -Force ".tools/wix", "dist" | Out-Null
    if (Test-Path ".tools/wix/wix.exe") {
        dotnet tool update wix --version 7.0.1 --tool-path ".tools/wix"
    } else {
        dotnet tool install wix --version 7.0.1 --tool-path ".tools/wix"
    }
    $wix = Join-Path $root ".tools/wix/wix.exe"
    & $wix extension add WixToolset.UI.wixext/7.0.1 --acceptEula wix7
    & $wix build --acceptEula wix7 -arch x64 `
        -ext WixToolset.UI.wixext/7.0.1 `
        -d "SourceDir=$root" `
        -o "dist/oxeye-0.1.0-windows-x86_64.msi" `
        "packaging/windows/main.wxs"
} finally {
    $env:RUSTUP_TOOLCHAIN = $oldToolchain
    $env:RUSTFLAGS = $oldRustflags
    Pop-Location
}
