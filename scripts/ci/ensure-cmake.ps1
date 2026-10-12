# Windows validation setup. Dot-source to test the helpers without installing tools.
[CmdletBinding()]
param()

# Cargo.lock: aws-lc-sys 0.45.0 requires 3.5 in its CMakeLists.txt, but
# cmake 0.1.58 selects VS 2022 on windows-2022, making 3.21 the effective floor.
# https://github.com/aws/aws-lc-rs/blob/7943223c99d909bc399bdf1b856821bb04f1f3c5/aws-lc-sys/CMakeLists.txt
# https://github.com/rust-lang/cmake-rs/blob/63190697197b34e5e5cd3cabf17da4fcab348bef/src/lib.rs#L967
# https://cmake.org/cmake/help/latest/generator/Visual%20Studio%2017%202022.html
$script:MinimumCMakeVersion = [version]'3.21.0'

function ConvertTo-CMakeVersion {
    param([string]$Output)
    # Only accept a stable release banner, not a guessed version or an RC.
    if ($Output -match '(?m)^cmake version (\d+\.\d+\.\d+)\s*$') {
        return [version]$Matches[1]
    }
    return $null
}

function Get-CMakeInstallation {
    $command = Get-Command cmake -CommandType Application -ErrorAction SilentlyContinue
    if (-not $command) { return $null }
    try {
        $output = & $command.Source --version 2>&1
        if ($LASTEXITCODE -ne 0) { return $null }
        $version = ConvertTo-CMakeVersion ($output -join "`n")
        if ($null -eq $version) { return $null }
        return [pscustomobject]@{ Path = $command.Source; Version = $version }
    } catch {
        return $null
    }
}

function Install-CMake {
    $choco = Get-Command choco -CommandType Application -ErrorAction Stop
    # upgrade also installs an absent package, and unlike install it replaces an
    # already-installed but insufficient Chocolatey version instead of skipping it.
    & $choco.Source upgrade cmake --installargs 'ADD_CMAKE_TO_PATH=System' -y
    if ($LASTEXITCODE -ne 0) {
        throw "Chocolatey CMake installation failed (exit $LASTEXITCODE)."
    }
}

function Update-CMakeSearchPath {
    # Installer changes to machine/user PATH do not update this PowerShell process.
    # Prefer the MSI destination so an older PATH entry cannot shadow the new tool.
    $paths = @(
        (Join-Path $env:ProgramFiles 'CMake/bin'),
        [Environment]::GetEnvironmentVariable('Path', 'Machine'),
        [Environment]::GetEnvironmentVariable('Path', 'User'),
        $env:PATH
    ) | Where-Object { $_ }
    $env:PATH = $paths -join [IO.Path]::PathSeparator
}

function Use-CMakeInstallation {
    param($Installation)
    $directory = Split-Path -Parent $Installation.Path
    $env:PATH = $directory + [IO.Path]::PathSeparator + $env:PATH
    if ($env:GITHUB_PATH) {
        # GITHUB_PATH affects later steps; PATH above affects this step.
        Add-Content -LiteralPath $env:GITHUB_PATH -Value $directory -Encoding utf8
    }
    Write-Host "Using CMake $($Installation.Version) at $($Installation.Path)"
}

function Ensure-CMake {
    $installation = Get-CMakeInstallation
    if ($installation -and $installation.Version -ge $script:MinimumCMakeVersion) {
        Use-CMakeInstallation $installation
        return
    }

    Write-Host "CMake >= $script:MinimumCMakeVersion is unavailable; installing with Chocolatey."
    Install-CMake
    Update-CMakeSearchPath
    # Re-resolve the executable that subsequent build commands will actually use.
    $installation = Get-CMakeInstallation
    if (-not $installation -or $installation.Version -lt $script:MinimumCMakeVersion) {
        throw "CMake >= $script:MinimumCMakeVersion is still unavailable after installation."
    }
    Use-CMakeInstallation $installation
}

if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    Ensure-CMake
}
