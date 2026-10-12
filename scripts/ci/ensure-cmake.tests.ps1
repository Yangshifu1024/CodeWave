$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/ensure-cmake.ps1"

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

Assert-True ($script:MinimumCMakeVersion -eq [version]'3.21.0') 'VS 2022 requires CMake 3.21.'
foreach ($version in @('3.21.0', '3.31.6', '4.2.1')) {
    Assert-True ((ConvertTo-CMakeVersion "cmake version $version`n`nCMake suite maintained by Kitware") -eq [version]$version) "Parse $version"
}
foreach ($banner in @('', 'PowerShell 7.4.0', 'cmake version 3.21.0-rc1', 'cmake version 3.21')) {
    Assert-True ($null -eq (ConvertTo-CMakeVersion $banner)) "Reject invalid banner: $banner"
}

# Test command detection and native exit handling without downloading software.
function Get-Command {
    param($Name, $CommandType, $ErrorAction)
    Assert-True ($CommandType -eq 'Application') 'Only resolve applications.'
    if ($script:MissingCommand) { return $null }
    return [pscustomobject]@{ Source = {
        $global:LASTEXITCODE = $script:NativeExit
        $script:NativeArguments = $args
        $script:NativeOutput
    } }
}
$script:MissingCommand = $true
Assert-True ($null -eq (Get-CMakeInstallation)) 'Absent CMake must be unavailable.'
$script:MissingCommand = $false
$script:NativeExit = 0
$script:NativeOutput = 'cmake version 3.31.6'
Assert-True ((Get-CMakeInstallation).Version -eq [version]'3.31.6') 'Detect version from resolved executable.'
Assert-True ($script:NativeArguments[0] -eq '--version') 'Invoke resolved CMake with --version.'
$script:NativeExit = 1
Assert-True ($null -eq (Get-CMakeInstallation)) 'Nonzero --version must be unavailable.'
$script:NativeExit = 0
$script:NativeOutput = 'invalid banner'
Assert-True ($null -eq (Get-CMakeInstallation)) 'Unrecognized --version must be unavailable.'

$script:NativeOutput = $null
Install-CMake
Assert-True (($script:NativeArguments -join '|') -eq 'upgrade|cmake|--installargs|ADD_CMAKE_TO_PATH=System|-y') 'Fallback must install or upgrade CMake.'
$script:NativeExit = 1
$failed = $false
try { Install-CMake } catch { $failed = $true }
Assert-True $failed 'Installer failure must stop setup.'

# Exercise the real PATH publisher separately from the setup state-machine mocks.
$oldPath = $env:PATH
$oldGithubPath = $env:GITHUB_PATH
$oldProgramFiles = $env:ProgramFiles
$pathFile = [IO.Path]::GetTempFileName()
try {
    $env:ProgramFiles = Join-Path ([IO.Path]::GetTempPath()) 'Program Files'
    Update-CMakeSearchPath
    $expectedFirst = Join-Path $env:ProgramFiles 'CMake/bin'
    Assert-True ($env:PATH.StartsWith($expectedFirst + [IO.Path]::PathSeparator)) 'Installer destination must precede stale PATH entries.'
    Assert-True ($env:PATH.EndsWith($oldPath)) 'Refresh must preserve existing process PATH entries.'
    $env:GITHUB_PATH = $pathFile
    $executable = Join-Path ([IO.Path]::GetTempPath()) 'cmake-fixture/cmake.exe'
    $directory = Split-Path -Parent $executable
    Use-CMakeInstallation ([pscustomobject]@{ Path = $executable; Version = [version]'3.21.0' })
    Assert-True ($env:PATH.StartsWith($directory + [IO.Path]::PathSeparator)) 'Selected CMake must lead current PATH.'
    Assert-True ((Get-Content -LiteralPath $pathFile) -eq $directory) 'Selected CMake must persist to future steps.'
} finally {
    $env:PATH = $oldPath
    $env:GITHUB_PATH = $oldGithubPath
    $env:ProgramFiles = $oldProgramFiles
    Remove-Item -LiteralPath $pathFile
}

function Get-CMakeInstallation {
    $script:Checks += 1
    if ($script:Checks -eq 1) { return $script:Initial }
    return $script:AfterInstall
}
function Install-CMake { $script:Installs += 1 }
function Update-CMakeSearchPath { $script:Refreshes += 1 }
function Use-CMakeInstallation { param($Installation) $script:Selected = $Installation }

$sufficient = [pscustomobject]@{ Path = 'new/cmake.exe'; Version = [version]'3.21.0' }
$old = [pscustomobject]@{ Path = 'old/cmake.exe'; Version = [version]'3.20.6' }
foreach ($initial in @($null, $old, $sufficient)) {
    $script:Initial = $initial
    $script:AfterInstall = $sufficient
    $script:Checks = $script:Installs = $script:Refreshes = 0
    $script:Selected = $null
    Ensure-CMake
    $expectedInstalls = if ($initial -eq $sufficient) { 0 } else { 1 }
    Assert-True ($script:Installs -eq $expectedInstalls) 'Install only if absent or insufficient.'
    Assert-True ($script:Refreshes -eq $expectedInstalls) 'Refresh PATH only after installation.'
    Assert-True ($script:Checks -eq (1 + $expectedInstalls)) 'Re-resolve CMake after installation.'
    Assert-True ($script:Selected -eq $sufficient) 'Use the verified installation.'
}
foreach ($result in @($null, $old)) {
    $script:Initial = $null
    $script:AfterInstall = $result
    $script:Checks = $script:Installs = $script:Refreshes = 0
    $script:Selected = $null
    $failed = $false
    try { Ensure-CMake } catch { $failed = $true }
    Assert-True $failed 'Missing or insufficient post-install CMake must fail.'
    Assert-True ($null -eq $script:Selected) 'Never publish an unverified installation.'
}
Write-Host 'CMake setup tests passed.'
