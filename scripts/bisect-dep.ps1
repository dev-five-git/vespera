<#
.SYNOPSIS
  Bisect a performance (or any) regression across the published versions of ONE
  dependency, using `cargo update -p <crate> --precise <ver>`.

.DESCRIPTION
  Like `git bisect run`, but over crates.io releases instead of commits.
  The probe command decides good/bad by exit code: 0 = good, non-zero = bad.
  For a perf regression make the probe fail when the number is too high, e.g. a
  script that runs `cargo bench ... -- --baseline main` and applies the same
  rule as scripts/bench-interleaved.sh: the median of the paired per-round
  changes above +30%, with every round above +20%.

  - Candidate versions come from the crates.io API: non-yanked, non-prerelease
    releases between -Good and -Bad (inclusive), semver-sorted.
  - Each probe runs `cargo update -p <crate> --precise <candidate>`, so the crate
    must appear once in Cargo.lock and its Cargo.toml requirement must allow the
    candidate (widen it temporarily to cross a 0.x minor / major boundary).
  - Cargo.lock is backed up first and ALWAYS restored on exit.
  - -Good must really be good and -Bad really bad; they are not re-verified.
  - -Command is ONE command (a native command or a script path). PowerShell's
    `-Command` reports only its last statement's status, so `a; b` would hide a
    failing `a`; such probes are rejected. Chain steps in a script file instead.

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
# One command only: `powershell -Command` exits with its last statement's
# status, so a multi-statement probe would hide an earlier failure.
if ($Command -match '[;|\r\n]|&&') {
    throw "-Command must be a single command (no ';', '|', '&&' or newlines): the probe's exit code is its last statement's. Put multi-step probes in a script file."
}
$root = Split-Path -Parent $PSScriptRoot
$lock = Join-Path $root 'Cargo.lock'
$backup = "$lock.bisect-backup"

# -ErrorAction Stop: under 'Continue' a failed lookup (network error, unknown
# crate) would leave $resp empty and surface as the misleading Good/Bad error.
try {
    $resp = Invoke-RestMethod -Uri "https://crates.io/api/v1/crates/$Crate/versions" `
        -UserAgent 'vespera-bisect-dep (https://github.com/dev-five-git/vespera)' -ErrorAction Stop
} catch {
    throw "crates.io lookup failed for '$Crate' (network error or unknown crate): $($_.Exception.Message)"
}
$all = @($resp.versions | Where-Object { -not $_.yanked -and $_.num -notmatch '-' } |
    ForEach-Object { [version]($_.num -replace '\+.*$', '') } | Sort-Object -Unique)
if ($all.Count -eq 0) {
    throw "crates.io lists no stable, non-yanked release of '$Crate'."
}
$lo = [array]::IndexOf($all, [version]$Good)
$hi = [array]::IndexOf($all, [version]$Bad)
if ($lo -lt 0) { throw "'$Good' is not a published non-yanked release of $Crate." }
if ($hi -lt 0) { throw "'$Bad' is not a published non-yanked release of $Crate." }
if ($lo -ge $hi) {
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
        # -Command re-parses the probe text: on Windows PowerShell 5.1 it drops
        # embedded double quotes and collapses runs of spaces, and a probe that
        # then fails to parse exits non-zero (read as "bad"). Pass the text
        # base64-encoded (UTF-16LE) instead, with progress output silenced so
        # the child's stderr stays clean.
        $probe = '$ProgressPreference = ''SilentlyContinue''; ' + $Command
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
        Push-Location $root
        try { powershell -NoProfile -EncodedCommand $encoded; $rc = $LASTEXITCODE } finally { Pop-Location }
        if ($rc -eq 0) { $lo = $mid } else { $hi = $mid }
    }
    Write-Host "RESULT: last good $Crate $($all[$lo]); first bad $Crate $($all[$hi])"
}
finally {
    Move-Item $backup $lock -Force
}
