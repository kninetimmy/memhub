# Exercise the runner's fail-closed checks without attaching or formatting any disk.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$errors = $null
$tokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'test-windows.ps1'), [ref] $tokens, [ref] $errors)
if ($errors.Count) { throw "Invalid runner script: $errors" }
foreach ($name in @('Assert-PlainPath', 'Assert-OwnedPaths', 'Assert-FixtureIdentity', 'Get-OwnedDisk', 'Invoke-CheckedTests', 'Invoke-FixtureRun')) {
    $function = $ast.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $false)
    . ([scriptblock]::Create($function.Extent.Text))
}

function Assert-Refusal([scriptblock] $Action, [string] $Message) {
    try { & $Action } catch {
        if ($_.Exception.Message.Contains($Message)) { return }
        throw
    }
    throw "Expected refusal containing: $Message"
}

$temp = [IO.Path]::GetTempPath().TrimEnd('\')
$owner = [guid]::NewGuid().ToString('N')
$owned = Join-Path $temp "memhub-prune-checks-$owner"
$image = Join-Path $owned 'fixture.vhd'
$marker = Join-Path $owned 'owner.txt'
$null = New-Item -ItemType Directory -Path $owned
try {
    Set-Content -LiteralPath $marker -Value $owner
    Assert-OwnedPaths
    Set-Content -LiteralPath $marker -Value 'not-the-owner'
    Assert-Refusal { Assert-OwnedPaths } 'ownership/containment check failed'
    Set-Content -LiteralPath $marker -Value $owner
    $image = Join-Path $temp 'outside.vhd'
    Assert-Refusal { Assert-OwnedPaths } 'ownership/containment check failed'
    $image = Join-Path $owned 'fixture.vhd'
    $link = Join-Path $owned 'linked'
    $null = New-Item -ItemType Junction -Path $link -Target $owned
    Assert-Refusal { Assert-PlainPath $link } 'reparse point'
} finally {
    # Delete only the exact files/link this check created, without recursive traversal.
    if (Test-Path -LiteralPath (Join-Path $owned 'linked')) {
        [IO.Directory]::Delete((Join-Path $owned 'linked'))
    }
    Remove-Item -LiteralPath $marker
    [IO.Directory]::Delete($owned)
}

$image = 'X:\job-owned\fixture.vhd'
$diskPath = 'fixture-device-path'
$existingDisks = @(0, 1)
$script:pathsChecked = 0
function Assert-OwnedPaths { $script:pathsChecked++ }
$mounted = [pscustomobject]@{ Attached = $true; ImagePath = $image }
$disk = [pscustomobject]@{
    Number = 2; IsBoot = $false; IsSystem = $false
    BusType = 'File Backed Virtual'; Size = 256MB; Path = $diskPath
    CimInstanceProperties = @{ BusType = [pscustomobject]@{ Value = [uint16] 15 } }
}
function Get-DiskImage { param($ImagePath) return $mounted }
function Get-Disk { process { return $disk } }
if ((Get-OwnedDisk).Number -ne 2 -or $script:pathsChecked -ne 1) { throw 'Valid disk rejected or paths unchecked.' }
foreach ($property in @('Number', 'IsBoot', 'IsSystem', 'Size', 'Path')) {
    $original = $disk.$property
    $disk.$property = switch ($property) {
        Number { 0 }; IsBoot { $true }; IsSystem { $true }
        Size { 1TB }; Path { 'different-device' }
    }
    $check = switch ($property) { Number { 'NumberNotPreexisting' }; Path { 'DevicePath' }; default { $property } }
    Assert-Refusal { Get-OwnedDisk } "Fixture identity check '$check' failed: actual="
    $disk.$property = $original
}
# The display label cannot authorize a wrong, missing or untyped raw storage enum.
foreach ($bad in @([uint16] 11, [uint16] 17, 15, '15', 'File Backed Virtual', $null)) {
    $disk.CimInstanceProperties['BusType'].Value = $bad
    Assert-Refusal { Get-OwnedDisk } "Fixture identity check 'BusType' failed: actual="
}
$busProperty = $disk.CimInstanceProperties['BusType']
$disk.CimInstanceProperties.Remove('BusType')
Assert-Refusal { Get-OwnedDisk } "Fixture identity check 'BusType' failed: actual=null"
$busProperty.Value = [uint16] 15
$disk.CimInstanceProperties['BusType'] = $busProperty
$singleDisk = $disk
$disk = @()
Assert-Refusal { Get-OwnedDisk } 'exactly one owned virtual disk'
$disk = @($singleDisk, $singleDisk)
Assert-Refusal { Get-OwnedDisk } 'exactly one owned virtual disk'
$disk = $singleDisk
$mounted.ImagePath = 'X:\some-other.vhd'
Assert-Refusal { Get-OwnedDisk } 'expected image path'
$mounted.ImagePath = $image
$mounted.Attached = $false
Assert-Refusal { Get-OwnedDisk } 'not attached'
Assert-Refusal { Assert-FixtureIdentity 'Sample' $false ([uint16] 15) 15 } 'actual=15 (type=System.UInt16); expected=15'

# Preserve the first error even when teardown also refuses, and never hide teardown-only failures.
$messages = @()
Assert-Refusal {
    Invoke-FixtureRun { throw 'original setup failure' } { throw 'separate teardown failure' } 6>&1 |
        ForEach-Object { $script:messages += "$_" }
} 'original setup failure'
foreach ($message in @('Fixture setup/tests failed: original setup failure', 'Fixture teardown failed: separate teardown failure')) {
    if ($message -notin $messages) { throw "Missing separate error report: $message" }
}
Assert-Refusal { Invoke-FixtureRun {} { throw 'teardown-only failure' } 6> $null } 'teardown-only failure'
$script:sequence = @()
Invoke-FixtureRun { $script:sequence += 'action'; $fixtureState = 'owned' } {
    if ($fixtureState -ne 'owned') { throw 'Fixture state did not survive until teardown.' }
    $script:sequence += 'teardown'
}
if (($script:sequence -join ',') -ne 'action,teardown') { throw 'Fixture run order is incorrect.' }

# Mock cargo only; exercise the real coverage and exit-status verifier.
$script:cargoExit = 0
$script:lines = @(
    'a_backup_inspection_error_omits_only_the_incomplete_group',
    'backup_cleanup_requires_confirmation_and_only_removes_reviewed_surplus',
    'backup_cleanup_refuses_before_any_deletion_when_a_keeper_or_victim_is_busy',
    'backup_cleanup_with_an_attribute_reader_is_complete_or_refused',
    'backup_cleanup_requires_a_new_review_after_an_identical_file_replacement',
    'backup_cleanup_skips_linked_children_and_refuses_a_linked_backup_root',
    'storage::windows_ci_refuses_missing_or_unusable_controlled_storage'
) | ForEach-Object { "test prune::$_ ... ok" }
function cargo {
    $script:receivedArguments = $args
    $global:LASTEXITCODE = $script:cargoExit
    $script:lines
}
Invoke-CheckedTests -CargoArguments @('test', '--locked', '--test', 'lifecycle_harness', 'prune::') 6> $null
if (($script:receivedArguments[0..4] -join ' ') -ne 'test --locked --test lifecycle_harness prune::') {
    throw 'Focused cargo arguments were not forwarded.'
}
$passed = $script:lines
foreach ($bad in @(@(), @($passed[0]), @($passed + $passed[0]), @($passed -replace 'ok$', 'ignored'))) {
    $script:lines = $bad
    Assert-Refusal { Invoke-CheckedTests -CargoArguments @('test', '--locked') 6> $null } 'did not run and pass exactly once'
}
$script:lines = $passed
$script:cargoExit = 1
Assert-Refusal { Invoke-CheckedTests -CargoArguments @('test', '--locked') 6> $null } 'failed: 1'
$global:LASTEXITCODE = 0
Write-Host 'Windows runner disk-identity and coverage checks passed (no storage operations).'
