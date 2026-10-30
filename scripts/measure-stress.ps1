<#
.SYNOPSIS
  Measure vespera macro build cost vs. route count N (scaling curve).

.DESCRIPTION
  For each N: generate the stress app (scripts/gen-stress-app.ps1), then
  -Runs times: clean-rebuild only the stress crate (deps stay built) via
  `cargo rustc -- -Z time-passes` (RUSTC_BOOTSTRAP, stable toolchain), and
  report wall seconds, the `macro_expand_crate` pass seconds, peak working
  set (MB, sampled every 25 ms across all rustc processes) and openapi.json
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
$env:RUSTC_BOOTSTRAP = "1"
$targetDir = Join-Path $repo "target"

function Invoke-Measured([string]$manifest, [string[]]$rustcArgs) {
  $psi = New-Object Diagnostics.ProcessStartInfo
  $psi.FileName = "cargo"
  $psi.Arguments = "rustc --quiet --manifest-path `"$manifest`" --target-dir `"$targetDir`" -- " + ($rustcArgs -join " ")
  $psi.RedirectStandardError = $true
  $psi.RedirectStandardOutput = $true
  $psi.UseShellExecute = $false
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = [Diagnostics.Process]::Start($psi)
  $errTask = $p.StandardError.ReadToEndAsync()
  $outTask = $p.StandardOutput.ReadToEndAsync()
  $peak = 0.0
  while (-not $p.HasExited) {
    $ws = (Get-Process -Name rustc -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum
    # peak of a single rustc is what matters; sum approximates concurrent crates
    $one = (Get-Process -Name rustc -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Maximum).Maximum
    if ($one -and $one / 1MB -gt $peak) { $peak = $one / 1MB }
    Start-Sleep -Milliseconds 25
  }
  $sw.Stop()
  [pscustomobject]@{ Wall = $sw.Elapsed.TotalSeconds; PeakMB = $peak; Err = $errTask.Result + $outTask.Result }
}

function Get-Pass([string]$text, [string]$pass) {
  foreach ($l in $text -split "`n") {
    if ($l -match "time:\s+([0-9.]+);\s+rss:\s+(\d+)MB\s+->\s+(\d+)MB.*\s$pass\s*$") {
      return @([double]$Matches[1], [int]$Matches[3])
    }
  }
  return @($null, $null)
}

function Median($xs) { $s = @($xs | Sort-Object); $s[[int][Math]::Floor(($s.Count - 1) / 2)] }

$rows = @()
foreach ($n in $Sizes) {
  $dir = & "$PSScriptRoot\gen-stress-app.ps1" -Routes $n -Control:$Control -Keep $Keep
  $manifest = Join-Path $dir "Cargo.toml"
  Remove-Item "$dir\openapi.json" -ErrorAction SilentlyContinue
  # warm: build deps + one pass so only the stress crate is measured
  [void](Invoke-Measured $manifest @("-Z", "time-passes"))
  $wall = @(); $exp = @(); $peak = @(); $rss = @()
  for ($r = 0; $r -lt $Runs; $r++) {
    Remove-Item "$dir\target\vespera" -Recurse -Force -ErrorAction SilentlyContinue
    & cargo clean --quiet -p stress-app --manifest-path $manifest --target-dir $targetDir 2>$null
    $m = Invoke-Measured $manifest @("-Z", "time-passes")
    $pass = Get-Pass $m.Err "macro_expand_crate"
    if ($null -eq $pass[0]) { throw "no macro_expand_crate pass in output:`n$($m.Err)" }
    $wall += $m.Wall; $exp += $pass[0]; $peak += $m.PeakMB; $rss += $pass[1]
  }
  & cargo clean --quiet -p stress-app --manifest-path $manifest --target-dir $targetDir 2>$null
  $hit = Invoke-Measured $manifest @("-Z", "time-passes")
  $hitExp = (Get-Pass $hit.Err "macro_expand_crate")[0]
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
