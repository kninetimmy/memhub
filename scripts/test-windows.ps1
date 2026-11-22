# Only the hosted Windows lane may create/format this disposable virtual disk.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $false
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Disposable prune storage is restricted to GitHub-hosted jobs.'
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Hosted fixture setup requires an already-administrative account; never elevate.'
}

function Assert-PlainPath([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force
    while ($null -ne $item) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Refusing a reparse point in fixture storage: $($item.FullName)"
        }
        # FileInfo has Directory, not Parent.
        if ($item -is [IO.FileInfo]) { $item = $item.Directory } else { $item = $item.Parent }
    }
}

$temp = (Get-Item -LiteralPath $env:RUNNER_TEMP -Force).FullName
Assert-PlainPath $temp
$owner = [guid]::NewGuid().ToString('N')
$owned = Join-Path $temp "memhub-prune-$owner"
$image = Join-Path $owned 'fixture.vhd'
# DiskPart reads a command file: reject quotes/newlines rather than interpolate them.
if ($image -notmatch '^[A-Za-z]:\\[A-Za-z0-9 _\\.\-]+$') {
    throw 'Unexpected hosted temp path; refusing to construct DiskPart input.'
}
$null = New-Item -ItemType Directory -Path $owned
$marker = Join-Path $owned 'owner.txt'
$create = Join-Path $owned 'create.txt'
Set-Content -LiteralPath $marker -Value $owner
Set-Content -LiteralPath $create -Encoding ascii -Value @"
create vdisk file="$image" maximum=256 type=expandable
exit
"@
$existingDisks = @(Get-Disk | Select-Object -ExpandProperty Number)
$diskPath = $null
$previousRoot = $env:MEMHUB_TEST_PRUNE_ROOT

function Assert-OwnedPaths {
    Assert-PlainPath $owned
    Assert-PlainPath $marker
    if ((Split-Path -Parent $owned) -ne $temp -or
        (Get-Content -LiteralPath $marker -Raw).Trim() -ne $owner -or
        (Split-Path -Parent $image) -ne $owned) {
        throw 'Fixture ownership/containment check failed.'
    }
    if (Test-Path -LiteralPath $image) { Assert-PlainPath $image }
}

function Assert-FixtureIdentity([string] $Name, [bool] $Matches, $Actual, $Expected) {
    if (-not $Matches) {
        $type = if ($null -eq $Actual) { 'null' } else { $Actual.GetType().FullName }
        throw "Fixture identity check '$Name' failed: actual=$(ConvertTo-Json -InputObject $Actual -Compress) (type=$type); expected=$(ConvertTo-Json -InputObject $Expected -Compress). Refusing storage operations."
    }
}

function Get-OwnedDisk {
    Assert-OwnedPaths
    $mounted = Get-DiskImage -ImagePath $image
    if (-not $mounted.Attached -or $mounted.ImagePath -ne $image) {
        throw 'The owned VHD is not attached at its expected image path.'
    }
    $disks = @($mounted | Get-Disk)
    if ($disks.Count -ne 1) { throw 'Expected exactly one owned virtual disk.' }
    $current = $disks[0]
    # STORAGE_BUS_TYPE 15 is FileBackedVirtual (not a host SATA/NVMe disk).
    Assert-FixtureIdentity 'NumberNotPreexisting' ($current.Number -notin $existingDisks) $current.Number "not in [$($existingDisks -join ', ')]"
    Assert-FixtureIdentity 'IsBoot' (-not $current.IsBoot) $current.IsBoot $false
    Assert-FixtureIdentity 'IsSystem' (-not $current.IsSystem) $current.IsSystem $false
    if ($current.BusType -ne 15 -and $null -ne $current.PSObject.Properties['CimInstanceProperties']) {
        $rawBusType = $current.CimInstanceProperties['BusType'].Value
        Write-Host "[DEBUG-469] BusType CIM value: actual=$rawBusType (type=$($rawBusType.GetType().FullName)); expected=15."
    }
    Assert-FixtureIdentity 'BusType' ($current.BusType -eq 15) $current.BusType 15
    Assert-FixtureIdentity 'Size' ($current.Size -eq 256MB) $current.Size 256MB
    Assert-FixtureIdentity 'DevicePath' ($null -eq $diskPath -or $current.Path -eq $diskPath) $current.Path $diskPath
    return $current
}

function Invoke-CheckedTests([string[]] $CargoArguments) {
    $output = @()
    & cargo @CargoArguments -- --format pretty --color never 2>&1 |
        Tee-Object -Variable output | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { throw "cargo $CargoArguments failed: $LASTEXITCODE" }
    # A successful cargo invocation with zero tests (or ignored cases) is not coverage.
    foreach ($name in @(
        'a_backup_inspection_error_omits_only_the_incomplete_group',
        'backup_cleanup_requires_confirmation_and_only_removes_reviewed_surplus',
        'backup_cleanup_refuses_before_any_deletion_when_a_keeper_or_victim_is_busy',
        'backup_cleanup_with_an_attribute_reader_is_complete_or_refused',
        'backup_cleanup_requires_a_new_review_after_an_identical_file_replacement',
        'backup_cleanup_skips_linked_children_and_refuses_a_linked_backup_root',
        'storage::windows_ci_refuses_missing_or_unusable_controlled_storage'
    )) {
        if (@($output | Where-Object { "$_" -eq "test prune::$name ... ok" }).Count -ne 1) {
            throw "Required Windows prune test did not run and pass exactly once: $name"
        }
    }
}

function Invoke-FixtureRun([scriptblock] $Action, [scriptblock] $Teardown) {
    $failure = $null
    try { . $Action } catch {
        $failure = $_
        Write-Host "Fixture setup/tests failed: $($_.Exception.Message)"
    } finally {
        try { . $Teardown } catch {
            Write-Host "Fixture teardown failed: $($_.Exception.Message)"
            if ($null -eq $failure) { $failure = $_ }
        }
    }
    if ($null -ne $failure) { throw $failure }
}

Invoke-FixtureRun -Action {
    Assert-OwnedPaths
    & diskpart /s $create
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $image -PathType Leaf)) {
        throw 'Failed to create the owned fixture VHD.'
    }
    Assert-OwnedPaths
    $null = Mount-DiskImage -ImagePath $image -NoDriveLetter
    $disk = Get-OwnedDisk
    $diskPath = $disk.Path
    if ($disk.PartitionStyle -ne 'RAW') { throw 'New fixture disk is not blank.' }
    $null = Initialize-Disk -InputObject (Get-OwnedDisk) -PartitionStyle GPT
    $partition = New-Partition -InputObject (Get-OwnedDisk) -UseMaximumSize -AssignDriveLetter
    $disk = Get-OwnedDisk
    if ($partition.DiskNumber -ne $disk.Number -or -not $partition.DriveLetter) {
        throw 'Fixture partition does not belong to the owned VHD.'
    }
    $null = Format-Volume -Partition $partition -FileSystem NTFS -NewFileSystemLabel 'memhub-prune' -Confirm:$false
    $root = "$($partition.DriveLetter):\"
    $mapped = Get-Partition -DriveLetter $partition.DriveLetter
    $disk = Get-OwnedDisk
    if ($mapped.DiskNumber -ne $disk.Number -or
        $mapped.PartitionNumber -ne $partition.PartitionNumber -or
        (Get-Volume -DriveLetter $partition.DriveLetter).FileSystem -ne 'NTFS') {
        throw 'Fixture drive mapping/filesystem verification failed.'
    }
    $env:MEMHUB_TEST_PRUNE_ROOT = Join-Path $root 'fixtures'
    $null = New-Item -ItemType Directory -Path $env:MEMHUB_TEST_PRUNE_ROOT
    Write-Host "Controlled prune storage: $image -> $root (disk $($disk.Number))"
    Invoke-CheckedTests -CargoArguments @('test', '--locked', '--test', 'lifecycle_harness', 'prune::')
    # Keep the same root for the second invocation through the full suite.
    Invoke-CheckedTests -CargoArguments @('test', '--locked')
} -Teardown {
    $env:MEMHUB_TEST_PRUNE_ROOT = $previousRoot
    Assert-OwnedPaths
    if (Test-Path -LiteralPath $image) {
        $mounted = Get-DiskImage -ImagePath $image
        if ($mounted.Attached) {
            $null = Get-OwnedDisk
            Dismount-DiskImage -ImagePath $image
        }
        if ((Get-DiskImage -ImagePath $image).Attached) { throw 'Fixture VHD is still attached.' }
    }
    # Never recurse into a mounted filesystem or remove anything outside our new directory.
    foreach ($path in @($image, $create, $marker)) {
        if (Test-Path -LiteralPath $path) {
            Assert-OwnedPaths
            Assert-PlainPath $path
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Not an owned fixture file: $path" }
            Remove-Item -LiteralPath $path
        }
    }
    [IO.Directory]::Delete($owned)
    Write-Host 'Fixture teardown succeeded: VHD detached and owned files/directory removed.'
}
