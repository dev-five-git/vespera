<#
.SYNOPSIS
  Bisect a performance (or any) regression across the published versions of ONE
  dependency, using `cargo update -p <crate>@<ver> --precise <ver>`.

.DESCRIPTION
  Like `git bisect run`, but over crates.io releases instead of commits.
  The probe command decides good/bad by exit code: 0 = good, non-zero = bad.
  For a perf regression make the probe fail when the number is too high, e.g. a
  script that runs `cargo bench ... -- --baseline main` and applies the same
  +30% mean / +20% CI-lower-bound rule as .github/workflows/bench.yml.

  - Candidate versions come from the crates.io API: non-yanked, non-prerelease
    releases between -Good and -Bad (inclusive), semver-sorted.
  - Each probe runs `cargo update -p <crate> --precise <candidate>`, so the crate
    must appear once in Cargo.lock and its Cargo.toml requirement must allow the
    candidate (widen it temporarily to cross a 0.x minor / major boundary).
  - Cargo.lock is backed up first and ALWAYS restored on exit.
  - -Good must really be good and -Bad really bad; they are not re-verified.

.EXAMPLE
  ./scripts/bisect-dep.ps1 -Crate hyper -Good 1.8.0 -Bad 1.12.0 `
      -Command 'cargo test -p vespera_inprocess --test alloc_budget'

.EXAMPLE
  ./scripts/bisect-dep.ps1 -Crate tokio -Good 1.50.0 -Bad 1.54.1 `
      -Command './scripts/my-bench-gate.ps1'
#>
param(
    [Parameter(Mandatory)][string]$Crate,
    [Parameter(Mandatory)][string]$Good,
    [Parameter(Mandatory)][string]$Bad,
    [Parameter(Mandatory)][string]$Command
)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$lock = Join-Path $root 'Cargo.lock'
$backup = "$lock.bisect-backup"

$resp = Invoke-RestMethod -Uri "https://crates.io/api/v1/crates/$Crate/versions" `
    -UserAgent 'vespera-bisect-dep (https://github.com/dev-five-git/vespera)'
$all = $resp.versions | Where-Object { -not $_.yanked -and $_.num -notmatch '-' } |
    ForEach-Object { [version]($_.num -replace '\+.*$', '') } | Sort-Object -Unique
$lo = [array]::IndexOf($all, [version]$Good)
$hi = [array]::IndexOf($all, [version]$Bad)
if ($lo -lt 0 -or $hi -lt 0 -or $lo -ge $hi) {
    throw "Good ($Good) must be an older published release than Bad ($Bad) for '$Crate'."
}

Copy-Item $lock $backup -Force
try {
    while ($hi - $lo -gt 1) {
        $mid = [int][math]::Floor(($lo + $hi) / 2)
        $candidate = $all[$mid].ToString()
        Write-Host "==> probing $Crate $candidate (good<=$($all[$lo]), bad>=$($all[$hi]))"
        cargo update --manifest-path (Join-Path $root 'Cargo.toml') -p $Crate --precise $candidate
        if ($LASTEXITCODE -ne 0) { throw "cargo update to $candidate failed; widen the version requirement." }
        Push-Location $root
        try { powershell -NoProfile -Command $Command; $rc = $LASTEXITCODE } finally { Pop-Location }
        if ($rc -eq 0) { $lo = $mid } else { $hi = $mid }
    }
    Write-Host "RESULT: last good $Crate $($all[$lo]); first bad $Crate $($all[$hi])"
}
finally {
    Move-Item $backup $lock -Force
}
