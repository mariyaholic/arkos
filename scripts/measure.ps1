<#
Budget measurement on Windows: builds release, generates the fixture, runs scan, diff and tree
three times each and prints the median for every row of spec section 6. Run it from an elevated
prompt to include the MFT row. Peak memory is the highest PeakWorkingSet64 seen while the child
process runs.

usage: scripts/measure.ps1 [-Files 100000] [-Dirs 12000] [-Depth 12]
#>
param(
    [int]$Files = 100000,
    [int]$Dirs = 12000,
    [int]$Depth = 12,
    [int]$Seed = 1
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("arkos-measure-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$env:ARKOS_HOME = Join-Path $work 'home'

function Get-Median([double[]]$values) {
    $sorted = $values | Sort-Object
    return $sorted[[int][Math]::Floor(($sorted.Count - 1) / 2)]
}

# Run a program, return wall milliseconds, peak working set bytes and captured stdout.
function Measure-Run([string]$exe, [string[]]$arguments) {
    $out = Join-Path $work 'stdout.txt'
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $exe -ArgumentList $arguments -PassThru -NoNewWindow `
        -RedirectStandardOutput $out -RedirectStandardError (Join-Path $work 'stderr.txt')
    $peak = 0
    while (-not $p.HasExited) {
        try {
            $p.Refresh()
            if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 }
        } catch { }
        Start-Sleep -Milliseconds 20
    }
    $p.WaitForExit()
    $sw.Stop()
    if ($p.ExitCode -ne 0) { throw "$exe $arguments exited with $($p.ExitCode)" }
    return [pscustomobject]@{
        Ms   = $sw.Elapsed.TotalMilliseconds
        Peak = $peak
        Out  = (Get-Content -Raw $out)
    }
}

try {
    Push-Location $root
    cargo build --release --bin arkos --example gen_tree
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    Pop-Location

    $bin = Join-Path $root 'target\release\arkos.exe'
    $gen = Join-Path $root 'target\release\examples\gen_tree.exe'
    $tree = Join-Path $work 'tree'

    Write-Host "== fixture: files=$Files dirs=$Dirs depth=$Depth seed=$Seed"
    $manifest = & $gen --files $Files --dirs $Dirs --depth $Depth --seed $Seed --out $tree
    Write-Host $manifest
    $hash = ($manifest -split 'hash=')[1].Trim()

    $scan = @(); $peaks = @()
    1..3 | ForEach-Object {
        if (Test-Path $env:ARKOS_HOME) { Remove-Item -Recurse -Force $env:ARKOS_HOME }
        $r = Measure-Run $bin @('scan', '--volume', $tree, '--mode', 'walk')
        $scan += $r.Ms; $peaks += $r.Peak
    }
    $first = (Measure-Run $bin @('scan', '--volume', $tree, '--mode', 'walk', '--json')).Out | ConvertFrom-Json
    $snapA = $first.snapshot_id
    $rollup = $first.rollup_ms

    # Change about one file in a hundred, then take the second snapshot.
    $i = 0
    Get-ChildItem -Path $tree -Recurse -File | ForEach-Object {
        $i++
        if ($i % 100 -eq 0 -and $i -le 200000) {
            $fs = [System.IO.File]::Open($_.FullName, 'Open', 'Write')
            $fs.SetLength(1MB); $fs.Dispose()
        }
    }
    $second = (Measure-Run $bin @('scan', '--volume', $tree, '--mode', 'walk', '--json')).Out | ConvertFrom-Json
    $snapB = $second.snapshot_id

    $diff = @(); $treeMs = @()
    1..3 | ForEach-Object {
        $diff += (Measure-Run $bin @('diff', "$snapA", "$snapB", '--top', '50', '--json')).Ms
    }
    1..3 | ForEach-Object {
        $out = Join-Path $work "tree-$_.html"
        $treeMs += (Measure-Run $bin @('tree', '--snapshot', "$snapB", '--depth', '12', '--out', $out)).Ms
    }
    $treeBytes = (Get-Item (Join-Path $work 'tree-1.html')).Length

    $list = 1..10 | ForEach-Object { (Measure-Run $bin @('list')).Ms }

    $info = (Measure-Run $bin @('db', 'info', '--json')).Out | ConvertFrom-Json
    $binBytes = (Get-Item $bin).Length

    # MFT row: only on an elevated token and an NTFS volume root; the walk rows above cover the rest.
    $mft = 'not measured (needs an elevated prompt and a volume root)'
    $elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($elevated -and $env:ARKOS_MFT_VOLUME) {
        $mftRuns = 1..3 | ForEach-Object { (Measure-Run $bin @('scan', '--volume', $env:ARKOS_MFT_VOLUME, '--mode', 'mft')).Ms }
        $mft = "$([int](Get-Median $mftRuns)) ms   (gate < 10000 at 1M files)"
    }

    $medScan = Get-Median $scan
    Write-Host '== results (median of 3; list is median of 10)'
    Write-Host ("platform            " + [System.Environment]::OSVersion.VersionString)
    Write-Host "fixture hash        $hash"
    Write-Host ("walk files/s        {0}   (gate >= 200000, fails < 150000)" -f [int]($Files * 1000 / $medScan))
    Write-Host ("scan wall ms        {0}" -f [int]$medScan)
    Write-Host "rollup ms           $rollup   (gate < 3000 at 1M files)"
    Write-Host ("peak working set    {0} B   (gate <= 60000000)" -f [int64](Get-Median $peaks))
    Write-Host ("db bytes/snapshot   {0}   (gate <= 75000000 at 1M files)" -f [int64]($info.db_bytes / $info.counts.arkos_snapshots))
    Write-Host ("diff ms             {0}   (gate < 2000 at 1M files)" -f [int](Get-Median $diff))
    Write-Host ("tree ms             {0}   (gate < 4000 at 100k dirs)" -f [int](Get-Median $treeMs))
    Write-Host "tree output bytes   $treeBytes   (gate < 40000000)"
    Write-Host ("list cold ms        {0}   (gate < 200)" -f [int](Get-Median $list))
    Write-Host "binary bytes        $binBytes   (gate <= 8000000)"
    Write-Host "mft                 $mft"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
