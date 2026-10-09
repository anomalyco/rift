param(
    [Parameter(Mandatory = $true)] [string] $Volume,
    [Parameter(Mandatory = $true)] [string] $Bench,
    [int] $Samples = 5,
    [string[]] $Only = @('cargo', 'TypeScript')
)

$ErrorActionPreference = 'Stop'
# The clone profiler writes to stderr. That must not fail the step.
$PSNativeCommandUseErrorActionPreference = $false
$letter = $Volume.TrimEnd('\').TrimEnd(':')
$root = "${letter}:\bench"
New-Item -ItemType Directory -Force -Path $root | Out-Null
$results = Join-Path $env:RUNNER_TEMP 'windows-bench.csv'
'repo,method,sample,ms,files' | Set-Content -Path $results -Encoding utf8

$repos = [ordered]@{
    'cargo'      = 'https://github.com/rust-lang/cargo'
    'TypeScript' = 'https://github.com/microsoft/TypeScript'
}

function FileCount([string] $Path) {
    (Get-ChildItem -LiteralPath $Path -Recurse -Force -File -Attributes !ReparsePoint | Measure-Object).Count
}

function Time([scriptblock] $Block) {
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Block
    $watch.Stop()
    $watch.Elapsed.TotalMilliseconds
}

$selected = @($repos.Keys | Where-Object { $Only -contains $_ })
if ($selected.Count -eq 0) { throw "no bench repos matched: $($Only -join ', ')" }
Write-Output "==> repos: $($selected -join ', '), samples: $Samples"

foreach ($name in $selected) {
    $source = Join-Path $root $name
    git clone --quiet --depth 1 $repos[$name] $source
    if ($LASTEXITCODE -ne 0) { throw "clone of $name failed" }
    git -C $source status --porcelain | Out-Null
    $sourceFiles = FileCount $source
    $bytes = (Get-ChildItem -LiteralPath $source -Recurse -Force -File | Measure-Object -Sum Length).Sum
    Write-Output "==> $name at $source has $sourceFiles files, $([math]::Round($bytes / 1MB)) MiB"

    for ($sample = 1; $sample -le $Samples; $sample++) {
        $output = & $Bench $source --samples 1
        if ($LASTEXITCODE -ne 0) { throw "rift create benchmark failed: $output" }
        $line = $output | Where-Object { $_ -like 'create*' } | Select-Object -First 1
        $ms = [double](($line -split "`t")[2] -replace ' ms', '')
        "$name,rift-create,$sample,$ms,$sourceFiles" | Add-Content -Path $results -Encoding utf8
        Write-Output "$name rift-create $sample $ms ms"

        $worktree = Join-Path $root "$name-worktree-$sample"
        $ms = Time { git -C $source worktree add --quiet --detach $worktree | Out-Null }
        if ($LASTEXITCODE -ne 0) { throw "git worktree add failed" }
        $files = FileCount $worktree
        "$name,git-worktree-add,$sample,$ms,$files" | Add-Content -Path $results -Encoding utf8
        Write-Output "$name git-worktree-add $sample $ms ms ($files files)"
        git -C $source worktree remove --force $worktree
        git -C $source worktree prune

        $copy = Join-Path $root "$name-copy-$sample"
        $ms = Time { robocopy $source $copy /E /COPY:DAT /DCOPY:DAT /R:0 /W:0 /MT:1 /NFL /NDL /NJH /NJS /NP | Out-Null }
        if ($LASTEXITCODE -ge 8) { throw "robocopy failed with $LASTEXITCODE" }
        $files = FileCount $copy
        "$name,robocopy,$sample,$ms,$files" | Add-Content -Path $results -Encoding utf8
        Write-Output "$name robocopy $sample $ms ms ($files files)"
        cmd /c rd /s /q $copy
    }
}

Write-Output '==> summary (median, min, max in ms)'
Import-Csv $results | Group-Object repo, method | ForEach-Object {
    $values = $_.Group | ForEach-Object { [double]$_.ms } | Sort-Object
    $median = $values[[math]::Floor($values.Count / 2)]
    '{0}: median {1:N0} min {2:N0} max {3:N0} (n={4}, files={5})' -f $_.Name, $median, $values[0], $values[-1], $values.Count, $_.Group[0].files
}
