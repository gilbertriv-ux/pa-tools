#Requires -Version 7.0
<#
.SYNOPSIS
  Install the Pa Tools MSI, run what it installed, uninstall it, and check each step.

.DESCRIPTION
  A smoke test of the installer itself, for a machine that can run the MSI's architecture (the x64
  MSI on an x64 Windows runner). It installs software on the machine it runs on; run it from an
  elevated 64-bit PowerShell 7. In order it:

    1. reads the MSI's own database (the Property table) and checks that its ProductVersion is
       -ExpectedVersion, the version in Cargo.toml, and that the file name carries it too;
    2. installs the MSI silently, with a verbose Windows Installer log, under a time limit;
    3. finds pa-tools.exe through the App Paths key the MSI writes, checks that it sits where
       patools.wxs installs it (Program Files\Pa Tools), that every required file is there (the
       notice and licence texts among them, identical to the repository's), and that the product is
       listed under Apps and features (the Uninstall key) with the same version;
    4. runs `pa-tools.exe --version` and `pa-tools-cli.exe --version` and checks that each prints
       "Pa Tools" and -ExpectedVersion;
    5. uninstalls silently and checks that the install folder and both registry entries are gone.

  The version is not typed anywhere in this script or in patools.wxs: the caller reads it from
  Cargo.toml ([workspace.package] version, through `cargo metadata`), package.ps1 reads the same key
  to build the MSI, and this script compares what came out of each.

  The msiexec logs and the captured program output stay in -LogDir, so a workflow can upload them
  when this fails. On GitHub Actions a failure is also written as one error annotation, and a pass as
  one notice, so either can be read without opening the job log.

  Written for Pa Tools from the public documentation of msiexec, the Windows Installer automation
  objects, Start-Process and GitHub Actions:
    https://learn.microsoft.com/en-us/windows/win32/msi/command-line-options
    https://learn.microsoft.com/en-us/windows/win32/msi/standard-installer-command-line-options
    https://learn.microsoft.com/en-us/windows/win32/msi/installer-opendatabase
    https://learn.microsoft.com/en-us/windows/win32/msi/database-openview
    https://learn.microsoft.com/en-us/windows/win32/msi/view-execute
    https://learn.microsoft.com/en-us/windows/win32/msi/view-fetch
    https://learn.microsoft.com/en-us/windows/win32/msi/record-stringdata
    https://learn.microsoft.com/en-us/windows/win32/msi/productversion
    https://learn.microsoft.com/en-us/windows/win32/msi/uninstall-registry-key
    https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/start-process
    https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-commands

.PARAMETER Msi
  The installer to test.

.PARAMETER ExpectedVersion
  The workspace version (Cargo.toml). The MSI and both programs must carry it.

.PARAMETER LogDir
  Where the logs and captured output go (created if missing; msiexec will not create it).

.PARAMETER TimeoutSec
  How long an install or an uninstall may take before it is killed and the test fails.

.EXAMPLE
  pwsh packaging/windows/smoke-test.ps1 -Msi dist\x64\patools-0.11.0-windows-x64.msi `
    -ExpectedVersion 0.11.0 -LogDir $env:TEMP\msi-smoke
#>
param(
  [Parameter(Mandatory)] [string] $Msi,
  [Parameter(Mandatory)] [string] $ExpectedVersion,
  [Parameter(Mandatory)] [string] $LogDir,
  [int] $TimeoutSec = 600
)
$ErrorActionPreference = 'Stop'

# What patools.wxs installs: the folder "Pa Tools" under ProgramFiles6432Folder (Program Files, for a
# 64-bit package), and a default value in the App Paths key holding the full path of pa-tools.exe.
$ProductName = 'Pa Tools'
$InstallDir = Join-Path $env:ProgramFiles 'Pa Tools'
$AppPathsKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\pa-tools.exe'
$UninstallRoot = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ProgramsToRun = @('pa-tools.exe', 'pa-tools-cli.exe')
# Installed next to the programs, byte for byte what the repository holds (patools.wxs, LegalDir).
$LegalFiles = @('NOTICE', 'LICENSE-MIT', 'LICENSE-APACHE', 'ATTRIBUTION.md')

# The installed pa-tools.exe as the App Paths key records it, or $null.
function Get-RegisteredExe {
  $key = Get-Item -LiteralPath $AppPathsKey -ErrorAction SilentlyContinue
  if ($key) { return $key.GetValue('') }
  return $null
}

# The "Apps and features" entries named Pa Tools. Callers wrap the call in @( ) to get an array.
function Get-ArpEntries {
  Get-ChildItem -LiteralPath $UninstallRoot -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ItemProperty -LiteralPath $_.PSPath -ErrorAction SilentlyContinue } |
    Where-Object { $_.DisplayName -eq $ProductName }
}

# Late-bound calls on a Windows Installer automation object (a COM object with no type library
# PowerShell can bind to): the object's own type does the lookup by name.
function Invoke-ComMember($Target, [string] $Name, [System.Reflection.BindingFlags] $Kind, [object[]] $Arguments) {
  return $Target.GetType().InvokeMember($Name, $Kind, $null, $Target, $Arguments)
}

# Rows of a query on the MSI's database, each row an array of its fields as strings. Opens the
# database read-only (OpenDatabase mode 0), runs the query (OpenView, Execute) and reads every row
# (Fetch until it returns nothing, StringData(n) for field n, counted from 1).
function Get-MsiRows([string] $Path, [string] $Query, [int] $Columns) {
  $call = [System.Reflection.BindingFlags]::InvokeMethod
  $get = [System.Reflection.BindingFlags]::GetProperty
  $installer = $null; $database = $null; $view = $null
  $rows = [System.Collections.Generic.List[string[]]]::new()
  try {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = Invoke-ComMember $installer 'OpenDatabase' $call @($Path, 0)
    $view = Invoke-ComMember $database 'OpenView' $call @($Query)
    $null = Invoke-ComMember $view 'Execute' $call $null
    while ($true) {
      $record = Invoke-ComMember $view 'Fetch' $call $null
      if ($null -eq $record) { break }
      $fields = foreach ($n in 1..$Columns) { [string](Invoke-ComMember $record 'StringData' $get @($n)) }
      $rows.Add([string[]]@($fields))
    }
    $null = Invoke-ComMember $view 'Close' $call $null
  } finally {
    # Let go of the database file before msiexec opens it.
    foreach ($com in @($view, $database, $installer)) {
      if ($null -ne $com) { try { $null = [System.Runtime.InteropServices.Marshal]::ReleaseComObject($com) } catch { } }
    }
  }
  # Callers wrap the call in @( ) so that no rows, one row or many are all an array of rows.
  return $rows.ToArray()
}

# Run msiexec with no UI, no restart and a verbose log; fail on a bad exit code or on a timeout.
function Invoke-Msiexec([string] $What, [string[]] $MsiArgs, [string] $LogName) {
  $log = Join-Path $LogDir $LogName
  $argList = @($MsiArgs) + @('/qn', '/norestart', '/l*v', ('"{0}"' -f $log))
  Write-Host "==> msiexec $($argList -join ' ')"
  $proc = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') -ArgumentList $argList -PassThru -WindowStyle Hidden
  $null = $proc.Handle # holding the handle keeps ExitCode readable after the process has gone
  if (-not $proc.WaitForExit($TimeoutSec * 1000)) {
    $proc.Kill($true)
    throw "$What did not finish within $TimeoutSec s, so msiexec was stopped (log: $LogName)"
  }
  # 0 = done; 3010 = done, a restart would be needed (suppressed by /norestart).
  if ($proc.ExitCode -ne 0 -and $proc.ExitCode -ne 3010) {
    throw "$What failed: msiexec exited with $($proc.ExitCode) (log: $LogName)"
  }
  Write-Host "$What finished, msiexec exit code $($proc.ExitCode)"
}

# Run one installed program with --version, capture its output in files, return the text.
function Get-VersionText([string] $Exe) {
  $name = [IO.Path]::GetFileNameWithoutExtension($Exe)
  $outFile = Join-Path $LogDir "$name.version.out.txt"
  $errFile = Join-Path $LogDir "$name.version.err.txt"
  Write-Host "==> $Exe --version"
  $proc = Start-Process -FilePath $Exe -ArgumentList '--version' -PassThru -NoNewWindow `
    -RedirectStandardOutput $outFile -RedirectStandardError $errFile
  $null = $proc.Handle
  if (-not $proc.WaitForExit(60000)) {
    $proc.Kill($true)
    throw "$name --version did not finish within 60 s"
  }
  if ($proc.ExitCode -ne 0) { throw "$name --version exited with $($proc.ExitCode)" }
  $text = [string](Get-Content -LiteralPath $outFile -Raw)
  Write-Host "$name printed: $($text.Trim())"
  return $text
}

# One GitHub Actions workflow command with an escaped message (%, CR and LF are the characters the
# command syntax needs escaped).
function Write-Annotation([string] $Level, [string] $Title, [string] $Message) {
  $text = $Message.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A')
  Write-Host "::${Level} title=${Title}::${text}"
}

# One error annotation: the problem plus what the logs in $LogDir say about it.
function Write-ErrorAnnotation([string] $Problem) {
  $lines = [System.Collections.Generic.List[string]]::new()
  $lines.Add($Problem)
  foreach ($file in Get-ChildItem -LiteralPath $LogDir -File -ErrorAction SilentlyContinue) {
    $lines.Add("[$($file.Name)]")
    $failures = Select-String -LiteralPath $file.FullName -Pattern 'Return value 3|Error [0-9]+\.' -ErrorAction SilentlyContinue |
      Select-Object -First 5
    foreach ($hit in $failures) { $lines.Add($hit.Line.Trim()) }
    foreach ($tail in Get-Content -LiteralPath $file.FullName -Tail 6 -ErrorAction SilentlyContinue) { $lines.Add("$tail".Trim()) }
  }
  $clipped = $lines | ForEach-Object { if ($_.Length -gt 200) { $_.Substring(0, 200) } else { $_ } }
  Write-Annotation 'error' 'MSI smoke test failed' ($clipped -join "`n")
}

try {
  if (-not [Environment]::Is64BitProcess) { throw 'run this from a 64-bit PowerShell (the registry paths are for the 64-bit view)' }
  $msiPath = (Resolve-Path -LiteralPath $Msi).Path
  New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
  # An MSI ProductVersion is numeric (major.minor.build); package.ps1 drops a pre-release tag there.
  $msiVersion = ($ExpectedVersion -split '-')[0]

  # 1. What the MSI says about itself, before anything is installed.
  $props = @{}
  foreach ($row in @(Get-MsiRows $msiPath 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$row[0]] = $row[1] }
  Write-Host "MSI properties: ProductName='$($props['ProductName'])' ProductVersion='$($props['ProductVersion'])'"
  if ($props['ProductName'] -ne $ProductName) { throw "the MSI's ProductName is '$($props['ProductName'])', expected '$ProductName'" }
  if ($props['ProductVersion'] -ne $msiVersion) { throw "the MSI's ProductVersion is '$($props['ProductVersion'])', but Cargo.toml says $ExpectedVersion (expected '$msiVersion')" }
  if ([IO.Path]::GetFileName($msiPath) -notlike "*-$ExpectedVersion-*") { throw "the MSI's file name does not carry version ${ExpectedVersion}: $([IO.Path]::GetFileName($msiPath))" }
  # File.FileName is "short|long" when the long name is not 8.3.
  $packaged = @(Get-MsiRows $msiPath 'SELECT `FileName` FROM `File`' 1 | ForEach-Object { ($_[0] -split '\|')[-1] })
  Write-Host "MSI holds $($packaged.Count) files: $($packaged -join ', ')"
  foreach ($file in $LegalFiles) {
    if ($packaged -notcontains $file) { throw "$file is not in the MSI (it holds: $($packaged -join ', '))" }
  }

  if (Get-RegisteredExe) { throw 'Pa Tools is already installed on this machine; not touching it' }

  # 2. Install.
  Invoke-Msiexec -What 'install' -MsiArgs @('/i', ('"{0}"' -f $msiPath)) -LogName 'install.log'

  # 3. Find what was installed.
  $exe = Get-RegisteredExe
  if (-not $exe) { throw 'the install wrote no App Paths entry for pa-tools.exe' }
  $dir = Split-Path -Parent $exe
  Write-Host "installed in: $dir"
  if ($dir -ne $InstallDir) { throw "installed in '$dir', but patools.wxs installs into '$InstallDir'" }
  foreach ($file in ($ProgramsToRun + $LegalFiles)) {
    if (-not (Test-Path -LiteralPath (Join-Path $dir $file) -PathType Leaf)) { throw "$file is missing from $dir" }
  }
  foreach ($file in $LegalFiles) {
    $installed = (Get-FileHash -LiteralPath (Join-Path $dir $file) -Algorithm SHA256).Hash
    $repo = (Get-FileHash -LiteralPath (Join-Path $RepoRoot $file) -Algorithm SHA256).Hash
    if ($installed -ne $repo) { throw "the installed $file differs from the repository's" }
  }
  $arp = @(Get-ArpEntries)
  if ($arp.Count -ne 1) { throw "expected one '$ProductName' entry under Uninstall, found $($arp.Count)" }
  if ($arp[0].DisplayVersion -ne $msiVersion) { throw "Uninstall lists version '$($arp[0].DisplayVersion)', expected '$msiVersion'" }

  # 4. Run it.
  # The version as a whole word: not part of a longer number (10.11.0, 0.11.01), not followed by a
  # pre-release or build tag (0.11.0-rc.1, 0.11.0+x), not prefixed.
  $versionPattern = '(?<![0-9A-Za-z.+-])' + [regex]::Escape($ExpectedVersion) + '(?![0-9A-Za-z.+-])'
  foreach ($program in $ProgramsToRun) {
    $text = Get-VersionText (Join-Path $dir $program)
    if (-not $text.Contains($ProductName)) { throw "$program --version did not print '$ProductName': $text" }
    if ($text -notmatch $versionPattern) { throw "$program --version did not print version ${ExpectedVersion}: $text" }
  }

  # 5. Uninstall, and look for leftovers.
  Invoke-Msiexec -What 'uninstall' -MsiArgs @('/x', ('"{0}"' -f $msiPath)) -LogName 'uninstall.log'
  if (Test-Path -LiteralPath $dir) { throw "uninstall left $dir behind: $((Get-ChildItem -LiteralPath $dir -Name) -join ', ')" }
  if (Get-RegisteredExe) { throw 'uninstall left the App Paths entry behind' }
  if (@(Get-ArpEntries).Count -ne 0) { throw "uninstall left the '$ProductName' entry under Uninstall" }

  $summary = "installed, ran (version $ExpectedVersion) and removed $([IO.Path]::GetFileName($msiPath)) " +
    "($([IO.FileInfo]::new($msiPath).Length) bytes; ProductVersion $($props['ProductVersion']); files: $($packaged -join ', '))"
  Write-Host "MSI smoke test passed: $summary"
  if ($env:GITHUB_ACTIONS -eq 'true') { Write-Annotation 'notice' 'MSI smoke test passed' $summary }
} catch {
  $problem = $_.Exception.Message
  Write-Host "MSI smoke test FAILED: $problem"
  if ($env:GITHUB_ACTIONS -eq 'true') { Write-ErrorAnnotation $problem }
  exit 1
}
