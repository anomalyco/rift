param(
    [Parameter(Mandatory = $true)]
    [string] $Volume,
    [switch] $CreatePartition
)

$ErrorActionPreference = 'Stop'
$letter = $Volume.TrimEnd('\').TrimEnd(':')

function Fail([string] $Message) {
    Write-Output "::error::Windows filesystem fixture failed: $Message"
    exit 1
}

if ($CreatePartition) {
    Write-Output "==> create a ReFS partition at ${letter}:"
    $script = Join-Path $env:RUNNER_TEMP 'rift-refs-diskpart.txt'
    @(
        'select volume C:'
        'shrink desired=10240'
        'create partition primary'
        'format quick fs=refs label=RiftReFS'
        "assign letter=$letter"
    ) | Set-Content -Path $script -Encoding ascii
    diskpart /s $script
    if ($LASTEXITCODE -ne 0) { Fail "diskpart exited with $LASTEXITCODE" }
}

Write-Output "==> volume ${letter}:"
$info = Get-Volume -DriveLetter $letter
$info | Format-List DriveLetter, FileSystem, FileSystemLabel, AllocationUnitSize, Size, SizeRemaining | Out-String | Write-Output
fsutil fsinfo volumeinfo "${letter}:"
fsutil devdrv query "${letter}:"
if ($info.FileSystem -ne 'ReFS') { Fail "${letter}: is $($info.FileSystem), expected ReFS" }

Write-Output "==> copy the checkout"
$checkout = "${letter}:\rift"
robocopy $env:GITHUB_WORKSPACE $checkout /E /XD target /NFL /NDL /NJH /NJS /NP
if ($LASTEXITCODE -ge 8) { Fail "robocopy exited with $LASTEXITCODE" }
"RIFT_CHECKOUT=$checkout" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
exit 0
