<#
.SYNOPSIS
  Emit a standalone vespera "stress app" with N routes (macro scaling test).

.DESCRIPTION
  Writes a self-contained crate (own [workspace], so it never joins
  `cargo build --workspace`) under target/stress-app/rN by default. Mix per
  route i: a Req/Resp struct pair; every 10th a 12-variant enum; every 20th a
  `schema_type!` pick derivation; every 25th a 40-field struct; every 30th a
  self-referential (circular) node; every route references a shared 8-deep
  nesting chain and the generic `Page<T>` wrapper. 50 handlers per route file.
  Measure with scripts/measure-stress.ps1.
#>
param(
  [Parameter(Mandatory)] [int]$Routes,
  [string]$Out = "",
  [switch]$Control,
  [string]$Keep = ""
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $repo ("target\stress-app\" + $(if ($Control) { "c" } else { "r" }) + $Routes) }
$repoFwd = $repo -replace '\\', '/'
New-Item -ItemType Directory -Force -Path "$Out\src\routes", "$Out\src\models" | Out-Null

$toml = @"
[package]
name = "stress-app"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
vespera = { path = "$repoFwd/crates/vespera" }
serde = { version = "1", features = ["derive"] }
"@
[IO.File]::WriteAllText("$Out\Cargo.toml", $toml)
if (Test-Path "$repo\Cargo.lock") { Copy-Item "$repo\Cargo.lock" "$Out\Cargo.lock" -Force }

[IO.File]::WriteAllText("$Out\src\lib.rs", @"
#![allow(clippy::all, clippy::pedantic, unused)]
pub mod models;
mod routes;
use vespera::{axum, vespera};
pub fn create_app() -> axum::Router {
    vespera!(openapi = "openapi.json", title = "Stress", version = "1.0.0")
}
"@)

$hub = New-Object Text.StringBuilder
[void]$hub.AppendLine("use serde::{Deserialize, Serialize};`nuse vespera::Schema;`n")
for ($d = 7; $d -ge 0; $d--) {
  $inner = if ($d -eq 7) { "String" } else { "Deep$($d + 1)" }
  [void]$hub.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`n#[serde(rename_all = `"camelCase`")]`npub struct Deep$d { pub id: u64, pub label: String, pub inner: Option<Box<$(if ($d -eq 7) { 'String' } else { "Deep$($d + 1)" })>>, pub items: Vec<$inner> }`n")
}
[void]$hub.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`npub struct Page<T> { pub items: Vec<T>, pub total: u64, pub next: Option<String> }`n")
[IO.File]::WriteAllText("$Out\src\models\mod.rs", $hub.ToString())

$per = 50
$files = [Math]::Ceiling($Routes / $per)
$modLines = New-Object Text.StringBuilder
for ($f = 0; $f -lt $files; $f++) {
  [void]$modLines.AppendLine("pub mod g$f;")
  $sb = New-Object Text.StringBuilder
  [void]$sb.AppendLine("use serde::{Deserialize, Serialize};`nuse vespera::Schema;`nuse vespera::axum::Json;`nuse crate::models::{Deep0, Page};`n")
  $lo = $f * $per; $hi = [Math]::Min($Routes, $lo + $per) - 1
  for ($i = $lo; $i -le $hi; $i++) {
    [void]$sb.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`n#[serde(rename_all = `"camelCase`")]`npub struct Req$i { pub id: u64, pub name: String, pub note: Option<String>, pub tags: Vec<String>, pub score: f64, pub active: bool }`n")
    $extra = ""
    if ($i % 10 -eq 0) {
      $vars = (0..11 | ForEach-Object { "V$_" }) -join ", "
      [void]$sb.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`n#[serde(rename_all = `"snake_case`")]`npub enum Kind$i { #[default] $vars }`n")
      $extra += ", pub kind: Kind$i"
    }
    if ($i % 25 -eq 0) {
      $wide = (0..39 | ForEach-Object { "pub w$_`: String" }) -join ", "
      [void]$sb.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`npub struct Wide$i { $wide }`n")
      $extra += ", pub wide: Wide$i"
    }
    if ($i % 30 -eq 0) {
      [void]$sb.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`npub struct Node$i { pub id: u64, pub children: Vec<Node$i>, pub parent: Option<Box<Node$i>> }`n")
      $extra += ", pub node: Node$i"
    }
    [void]$sb.AppendLine("#[derive(Default, Clone, Serialize, Deserialize, Schema)]`n#[serde(rename_all = `"camelCase`")]`npub struct Resp$i { pub id: u64, pub req: Req$i, pub deep: Deep0$extra }`n")
    if ($i % 20 -eq 0) {
      [void]$sb.AppendLine("vespera::schema_type!(Slim$i from crate::routes::g$f::Resp$i, pick = [`"id`", `"deep`"]);`n")
    }
    $m = @("post", "put", "get")[$i % 3]
    if ($m -eq "get") {
      [void]$sb.AppendLine("#[vespera::route(get, path = `"/r$i`")]`npub async fn h$i() -> Json<Page<Resp$i>> { Json(Page::default()) }`n")
    } else {
      [void]$sb.AppendLine("#[vespera::route($m, path = `"/r$i`")]`npub async fn h$i(Json(body): Json<Req$i>) -> Json<Resp$i> { Json(Resp$i { req: body, ..Default::default() }) }`n")
    }
  }
  [IO.File]::WriteAllText("$Out\src\routes\g$f.rs", $sb.ToString())
}
[IO.File]::WriteAllText("$Out\src\routes\mod.rs", $modLines.ToString())

if ($Control) {
  foreach ($f in Get-ChildItem "$Out\src" -Recurse -Filter *.rs) {
    $t = [IO.File]::ReadAllText($f.FullName)
    $keepSet = $Keep -split "," 
    if ($keepSet -notcontains "schema") { $t = $t -replace ', Schema\)\]', ')]' -replace '(?m)^use vespera::Schema;\r?\n', '' -replace '(?m)^vespera::schema_type!.*\r?\n', '' }
    if ($keepSet -notcontains "route") { $t = $t -replace '(?m)^#\[vespera::route\(.*\r?\n', '' }
    if ($keepSet -notcontains "vespera") {
      $body = "pub fn create_app() {}"
      if ($keepSet -contains "axum") {
        $chain = (0..($Routes - 1) | ForEach-Object { $m = @("post", "put", "get")[$_ % 3]; "        .route(`"/r$_`", axum::routing::$m(routes::g$([Math]::Floor($_ / 50))::h$_))" }) -join "`n"
        $body = "pub fn create_app() -> axum::Router {`n    axum::Router::new()`n$chain`n}"
      }
      $t = [regex]::Replace($t, 'pub fn create_app\(\) -> axum::Router \{\r?\n\s*vespera!\([^\r\n]*\)\r?\n\}', { param($m) $body })
    }
    [IO.File]::WriteAllText($f.FullName, $t)
  }
}
Write-Output $Out
