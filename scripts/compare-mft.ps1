<#
Compares the experimental MFT reader with the walk on one volume root. It scans the root with
`--mode walk`, scans it again with `--mode mft --experimental`, runs `arkos diff` between the two
snapshots, and prints the file and directory counts, the total bytes, and the paths that exist on
one side only. Run it from an elevated prompt, and try a small USB stick or a mounted VHD first.

The two scans are taken a few seconds apart, so files that changed in between show up as gaps.
Expected gaps are listed in docs/user-guide.md (hardlinks, alternate data streams, NTFS metafiles
and folders the walk was denied).

usage: scripts/compare-mft.ps1 -Root X:\ [-Top 20] [-Arkos path\to\arkos.exe]
#>
param(
    [Parameter(Mandatory = $true)][string]$Root,
    [int]$Top = 20,
    [string]$Arkos = ''
)
$ErrorActionPreference = 'Stop'

if ($Root -notmatch '^[A-Za-z]:\\?$') {
    throw "-Root must be a volume root such as X:\ (the MFT reader cannot scan a subfolder), got '$Root'"
}
$volume = $Root.Substring(0, 2).ToUpper()

if ($Arkos -eq '') {
    $built = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\arkos.exe'
    if (Test-Path $built) { $Arkos = $built } else { $Arkos = 'arkos' }
}

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'run this script from an elevated prompt; the MFT reader needs an elevated token'
}

function Invoke-Arkos([string[]]$arguments) {
    $text = & $Arkos @arguments
    if ($LASTEXITCODE -ne 0) { throw "arkos $($arguments -join ' ') exited with $LASTEXITCODE" }
    return ($text -join "`n")
}

Write-Host "Scanning $volume with the walk ..."
$walk = Invoke-Arkos @('scan', '--volume', $volume, '--mode', 'walk', '--json') | ConvertFrom-Json
Write-Host "Scanning $volume with the experimental MFT reader ..."
$mft = Invoke-Arkos @('scan', '--volume', $volume, '--mode', 'mft', '--experimental', '--json') | ConvertFrom-Json
if ($mft.mode -ne 'mft') {
    Write-Warning "the MFT scan fell back to '$($mft.mode)'; see the arkos log for the reason"
}

'{0,-8} {1,12} {2,12} {3,18} {4,10} {5,8}' -f 'mode', 'files', 'dirs', 'bytes', 'denied', 'ms'
foreach ($s in @($walk, $mft)) {
    '{0,-8} {1,12} {2,12} {3,18} {4,10} {5,8}' -f $s.mode, $s.files, $s.dirs, $s.bytes, $s.denied, $s.scan_ms
}
'{0,-8} {1,12} {2,12} {3,18}' -f 'mft-walk', ($mft.files - $walk.files), ($mft.dirs - $walk.dirs), ($mft.bytes - $walk.bytes)

$diff = Invoke-Arkos @('diff', "$($walk.snapshot_id)", "$($mft.snapshot_id)", '--top', "$Top", '--json') | ConvertFrom-Json

# Diff A is the walk and B is the MFT scan: 'removed' is only in the walk, 'added' only in the MFT.
function Show-OneSided($rows, [string]$kind, [string]$label, [string]$sizeField) {
    $picked = @($rows | Where-Object { $_.kind -eq $kind } | Sort-Object { [math]::Abs($_.delta_bytes) } -Descending)
    Write-Host ''
    Write-Host "$label ($($picked.Count) shown, top $Top by size):"
    foreach ($r in $picked) {
        '  {0,16}  {1}' -f $r.$sizeField, $r.path
    }
}
Show-OneSided $diff.files 'removed' 'Files only in the walk' 'before_size'
Show-OneSided $diff.files 'added' 'Files only in the MFT scan' 'after_size'
Show-OneSided $diff.dirs 'removed' 'Folders only in the walk' 'before_bytes'
Show-OneSided $diff.dirs 'added' 'Folders only in the MFT scan' 'after_bytes'

Write-Host ''
Write-Host "Snapshots: walk #$($walk.snapshot_id), mft #$($mft.snapshot_id). Full diff: arkos diff $($walk.snapshot_id) $($mft.snapshot_id) --json"
