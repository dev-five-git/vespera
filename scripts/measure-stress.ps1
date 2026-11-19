<#
.SYNOPSIS
  Measure vespera macro build cost vs. route count N (scaling curve).

.DESCRIPTION
  For each N: generate the stress app (scripts/gen-stress-app.ps1), then
  -Runs times: clean-rebuild only the stress crate (deps stay built) via
  `cargo rustc -- -Z time-passes` (RUSTC_BOOTSTRAP, stable toolchain), and
  report wall seconds, the `macro_expand_crate` pass seconds, peak working
  set (MB, the largest single rustc process, sampled every 25 ms) and openapi.json
  size. Also one incremental rebuild (touch one route file). Prints a
  markdown table plus a log-log slope (1.0 = linear, >1 superlinear) of
  macro_expand_crate vs N.

  Example: ./scripts/measure-stress.ps1 -Sizes 10,100,500 -Runs 3
#>
param(
  [int[]]$Sizes = @(10, 100, 500),
  [int]$Runs = 3,
  [switch]$MacroStats,
  [switch]$Control,
  [string]$Keep = ""
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$targetDir = Join-Path $repo "target"

function Invoke-Measured([string]$manifest, [string[]]$rustcArgs) {
  $psi = New-Object Diagnostics.ProcessStartInfo
  $psi.FileName = "cargo"
  $psi.Arguments = "rustc --quiet --manifest-path `"$manifest`" --target-dir `"$targetDir`" -- " + ($rustcArgs -join " ")
  $psi.RedirectStandardError = $true
  $psi.RedirectStandardOutput = $true
  $psi.UseShellExecute = $false
  # Only the measured child sees RUSTC_BOOTSTRAP: the caller's shell keeps
  # its stable toolchain behaviour.
  $psi.EnvironmentVariables["RUSTC_BOOTSTRAP"] = "1"
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = [Diagnostics.Process]::Start($psi)
  $errTask = $p.StandardError.ReadToEndAsync()
  $outTask = $p.StandardOutput.ReadToEndAsync()
  $peak = 0.0
  while (-not $p.HasExited) {
    # The peak of the largest single rustc process of THIS build: only the
    # rustc children of the measured cargo (not every rustc on the machine,
    # e.g. rust-analyzer's), and never the sum over concurrent processes.
    $kids = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.Id)" -ErrorAction SilentlyContinue |
      Where-Object { $_.Name -eq "rustc.exe" }
    foreach ($kid in $kids) {
      $proc = Get-Process -Id $kid.ProcessId -ErrorAction SilentlyContinue
      if ($proc -and $proc.WorkingSet64 / 1MB -gt $peak) { $peak = $proc.WorkingSet64 / 1MB }
    }
    Start-Sleep -Milliseconds 25
  }
  $p.WaitForExit()
  $sw.Stop()
  $text = $errTask.Result + $outTask.Result
  # A failed build (the generated crate expands but does not type-check)
  # still prints its passes: never let its timings into the table / slope.
  if ($p.ExitCode -ne 0) {
    throw "cargo rustc failed for $manifest (exit $($p.ExitCode)):`n$text"
  }
  [pscustomobject]@{ Wall = $sw.Elapsed.TotalSeconds; PeakMB = $peak; Err = $text }
}

function Get-Pass([string]$text, [string]$pass) {
  foreach ($l in $text -split "`n") {
    if ($l -match "time:\s+([0-9.]+);\s+rss:\s+(\d+)MB\s+->\s+(\d+)MB.*\s$pass\s*$") {
      return @([double]$Matches[1], [int]$Matches[3])
    }
  }
  return @($null, $null)
}

# Where vespera! keeps its route cache for the stress crate: VESPERA_TARGET_DIR
# when set (a relative value is resolved against the crate's workspace root,
# the crate itself — it declares its own [workspace]), else $targetDir.
function VesperaCacheDir($crateDir) {
  $v = $env:VESPERA_TARGET_DIR
  if ([string]::IsNullOrEmpty($v)) { return Join-Path $targetDir "vespera" }
  if (-not [System.IO.Path]::IsPathRooted($v)) { $v = Join-Path $crateDir $v }
  return Join-Path $v "vespera"
}

# The middle value of an odd count; the mean of the two middle values of an even one.
function Median($xs) {
  $s = @($xs | Sort-Object)
  $n = $s.Count
  if ($n % 2 -eq 1) { return $s[[int](($n - 1) / 2)] }
  $upper = [int]($n / 2)
  return ($s[$upper - 1] + $s[$upper]) / 2
}

$rows = @()
foreach ($n in $Sizes) {
  $dir = & "$PSScriptRoot\gen-stress-app.ps1" -Routes $n -Control:$Control -Keep $Keep
  $manifest = Join-Path $dir "Cargo.toml"
  Remove-Item "$dir\openapi.json" -ErrorAction SilentlyContinue
  # warm: build deps + one pass so only the stress crate is measured
  [void](Invoke-Measured $manifest @("-Z", "time-passes"))
  $wall = @(); $exp = @(); $peak = @(); $rss = @()
  for ($r = 0; $r -lt $Runs; $r++) {
    # The route cache lives under the build's target dir (rustc's --out-dir),
    # i.e. $targetDir\vespera, or under VESPERA_TARGET_DIR when that is set.
    Remove-Item (VesperaCacheDir $dir) -Recurse -Force -ErrorAction SilentlyContinue
    & cargo clean --quiet -p stress-app --manifest-path $manifest --target-dir $targetDir 2>$null
    $m = Invoke-Measured $manifest @("-Z", "time-passes")
    $pass = Get-Pass $m.Err "macro_expand_crate"
    if ($null -eq $pass[0]) { throw "no macro_expand_crate pass in output:`n$($m.Err)" }
    $wall += $m.Wall; $exp += $pass[0]; $peak += $m.PeakMB; $rss += $pass[1]
  }
  & cargo clean --quiet -p stress-app --manifest-path $manifest --target-dir $targetDir 2>$null
  $hit = Invoke-Measured $manifest @("-Z", "time-passes")
  $hitExp = (Get-Pass $hit.Err "macro_expand_crate")[0]
  if ($null -eq $hitExp) { throw "no macro_expand_crate pass in cache-hit output:`n$($hit.Err)" }
  Add-Content "$dir\src\routes\g0.rs" "// touch"
  $inc = Invoke-Measured $manifest @("-Z", "time-passes")
  $oa = if (Test-Path "$dir\openapi.json") { [Math]::Round((Get-Item "$dir\openapi.json").Length / 1KB) } else { 0 }
  $stats = ""
  if ($MacroStats) {
    & cargo clean --quiet -p stress-app --manifest-path $manifest --target-dir $targetDir 2>$null
    $s = Invoke-Measured $manifest @("-Z", "macro-stats")
    $stats = $s.Err
    Set-Content (Join-Path $dir "macro-stats.txt") $stats
  }
  $rows += [pscustomobject]@{
    N = $n; WallS = [Math]::Round((Median $wall), 2); MacroExpandS = [Math]::Round((Median $exp), 3)
    MacroExpandMinS = [Math]::Round(($exp | Measure-Object -Minimum).Minimum, 3)
    PeakRustcMB = [Math]::Round((Median $peak)); RssAfterExpandMB = [Math]::Round((Median $rss))
    IncrWallS = [Math]::Round($inc.Wall, 2); CacheHitExpandS = [Math]::Round($hitExp, 3); OpenapiKB = $oa
  }
}
$rows | Format-Table -AutoSize
"| N | wall s | macro_expand s (median/min) | peak rustc MB | rss after expand MB | incr wall s | expand s (vespera cache hit) | openapi KB |"
"|--:|--:|--:|--:|--:|--:|--:|--:|"
foreach ($r in $rows) { "| $($r.N) | $($r.WallS) | $($r.MacroExpandS) / $($r.MacroExpandMinS) | $($r.PeakRustcMB) | $($r.RssAfterExpandMB) | $($r.IncrWallS) | $($r.CacheHitExpandS) | $($r.OpenapiKB) |" }
if ($rows.Count -ge 2) {
  $a = $rows[0]; $b = $rows[-1]
  $slope = [Math]::Log($b.MacroExpandMinS / $a.MacroExpandMinS) / [Math]::Log($b.N / $a.N)
  $slopeW = [Math]::Log($b.WallS / $a.WallS) / [Math]::Log($b.N / $a.N)
  "log-log slope N=$($a.N)->$($b.N): macro_expand_crate = {0:N2}, wall = {1:N2} (1.0 linear)" -f $slope, $slopeW
}
