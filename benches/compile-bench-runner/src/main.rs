//! Compile-time benchmark harness for vespera's proc-macros.
//!
//! Measures the `macro_expand_crate` rustc pass of a target fixture crate,
//! which isolates the cost of expanding `vespera!`, `schema_type!`, and
//! `#[derive(Schema)]` from the rest of compilation (type-check, codegen,
//! LTO). Runs on **stable** via `RUSTC_BOOTSTRAP=1` (no nightly required), so
//! it works in CI.
//!
//! ```text
//! cargo run -p compile-bench-runner --release -- [OPTIONS]
//!   --target <CRATE>      crate to measure         (default: macro-compile-bench)
//!   --pass <NAME>         -Z time-passes pass name (default: macro_expand_crate)
//!   --runs <N>            measured iterations      (default: 8)
//!   --save-baseline <X>   write samples to baselines/<X>.txt
//!   --baseline <X>        compare this run against baselines/<X>.txt
//! ```
//!
//! Methodology: each iteration runs `cargo clean -p <target>` and removes
//! `<target-dir>/vespera` and `<target-dir>/<triple>/vespera` (the route
//! cache) to force a full re-expansion, then `cargo rustc … -- -Z time-passes` and parses the pass
//! time. Compile time has only *positive* noise (a busy machine only ever
//! adds time), so `min` is the robust point estimate; `median`/`mean`/`sd`
//! are reported too. Gross outliers (> 3x median, e.g. AV/FS hiccups) are
//! dropped before stats.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Args {
    target: String,
    pass: String,
    runs: usize,
    save_baseline: Option<String>,
    baseline: Option<String>,
}

fn print_help() {
    eprint!(
        "compile-bench-runner — vespera proc-macro compile-time benchmark\n\n\
         USAGE: cargo run -p compile-bench-runner --release -- [OPTIONS]\n\
           --target <CRATE>      crate to measure        (default: macro-compile-bench)\n\
           --pass <NAME>         -Z time-passes pass     (default: macro_expand_crate)\n\
           --runs <N>            measured iterations     (default: 8)\n\
           --save-baseline <X>   save samples to baselines/<X>.txt\n\
           --baseline <X>        compare against baselines/<X>.txt\n\
           -h, --help            this help\n"
    );
}

fn parse_args() -> Args {
    let mut a = Args {
        target: "macro-compile-bench".to_owned(),
        pass: "macro_expand_crate".to_owned(),
        runs: 8,
        save_baseline: None,
        baseline: None,
    };
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = |flag: &str| {
            it.next()
                .unwrap_or_else(|| fatal(&format!("{flag} needs a value")))
        };
        match arg.as_str() {
            "--target" => a.target = next("--target"),
            "--pass" => a.pass = next("--pass"),
            "--runs" => {
                a.runs = next("--runs")
                    .parse()
                    .unwrap_or_else(|_| fatal("--runs must be an integer"));
            }
            "--save-baseline" => a.save_baseline = Some(next("--save-baseline")),
            "--baseline" => a.baseline = Some(next("--baseline")),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => fatal(&format!("unknown argument: {other} (try --help)")),
        }
    }
    if a.runs == 0 {
        fatal("--runs must be >= 1");
    }
    a
}

fn fatal(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2);
}

/// A `cargo` command pre-seeded with `RUSTC_BOOTSTRAP=1` so `-Z time-passes`
/// is accepted on a stable toolchain.
fn cargo() -> Command {
    let mut c = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    c.env("RUSTC_BOOTSTRAP", "1");
    c
}

/// Extract the seconds for `pass` from `-Z time-passes` stderr.
/// Lines look like: `time:   0.090; rss:   24MB ->   36MB ( +12MB)\tmacro_expand_crate`.
fn extract_pass_time(stderr: &str, pass: &str) -> Option<f64> {
    for line in stderr.lines() {
        let line = line.trim_end();
        if line.contains("time:") && line.split_whitespace().next_back() == Some(pass) {
            let after = line.split("time:").nth(1)?;
            return after.split(';').next()?.trim().parse::<f64>().ok();
        }
    }
    None
}

/// Where `vespera!` keeps its route cache (`routes-<tag>.cache`) and spec
/// sidecars.
#[derive(Debug, PartialEq)]
enum CacheDir {
    /// `<VESPERA_TARGET_DIR>/vespera` (a relative value resolved against the
    /// workspace root, as the macro does).
    Exact(PathBuf),
    /// Cargo's `target_directory`: the macro caches under
    /// `<target>/vespera`, or `<target>/<triple>/vespera` when a target
    /// triple is configured (`build.target` / `CARGO_BUILD_TARGET`; it reads
    /// the target directory off rustc's `--out-dir`).
    UnderTarget(PathBuf),
}

impl CacheDir {
    /// The cache directories to clear before a measured run: for a target
    /// directory, `<target>/vespera` plus every `<target>/<triple>/vespera`
    /// directory (re-listed each run: a triple directory appears with the
    /// first build). Only target-triple directories are descended into, so
    /// `<target>/doc/vespera` (rustdoc output) is left alone.
    fn dirs(&self) -> Vec<PathBuf> {
        match self {
            Self::Exact(dir) => vec![dir.clone()],
            Self::UnderTarget(target) => {
                let mut dirs = vec![target.join("vespera")];
                if let Ok(entries) = fs::read_dir(target) {
                    let mut nested: Vec<PathBuf> = entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| is_target_triple_dir(path))
                        .map(|triple| triple.join("vespera"))
                        .filter(|dir| dir.is_dir())
                        .collect();
                    nested.sort();
                    dirs.extend(nested);
                }
                dirs
            }
        }
    }
}

/// A target-triple directory (`<target>/<triple>/`) holds the profile
/// directories cargo builds into. Other directories directly under the target
/// directory (`doc`, `tmp`, ...) hold no build profile and are not triples.
fn is_target_triple_dir(path: &std::path::Path) -> bool {
    path.join("debug").is_dir() || path.join("release").is_dir()
}

/// [`CacheDir`] of the measured build: `VESPERA_TARGET_DIR` when set, else
/// cargo's `target_directory`.  `None` (with a warning) when neither is
/// known.
fn vespera_build_dir() -> Option<CacheDir> {
    let dir = locate_vespera_build_dir();
    if dir.is_none() {
        eprintln!("[warn] target directory unknown: vespera's route cache is not cleared");
    }
    dir
}

fn locate_vespera_build_dir() -> Option<CacheDir> {
    let metadata = || {
        let out = cargo()
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let override_dir = env::var_os("VESPERA_TARGET_DIR").filter(|dir| !dir.is_empty());
    build_dir_from(override_dir.map(PathBuf::from), metadata)
}

/// [`locate_vespera_build_dir`] over an optional `VESPERA_TARGET_DIR` and a
/// lazily read `cargo metadata` document.
fn build_dir_from(
    override_dir: Option<PathBuf>,
    metadata: impl FnOnce() -> Option<String>,
) -> Option<CacheDir> {
    match override_dir {
        Some(dir) if dir.is_absolute() => Some(CacheDir::Exact(dir.join("vespera"))),
        Some(dir) => Some(CacheDir::Exact(
            metadata_path(&metadata()?, "workspace_root")?
                .join(dir)
                .join("vespera"),
        )),
        None => metadata_path(&metadata()?, "target_directory").map(CacheDir::UnderTarget),
    }
}

/// The `"<field>"` path string of `cargo metadata` JSON (a path holds no
/// escapes other than `\\` and `\"`).
fn metadata_path(metadata: &str, field: &str) -> Option<PathBuf> {
    let rest = metadata.split(&format!("\"{field}\":\"")).nth(1)?;
    let mut dir = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(PathBuf::from(dir)),
            '\\' => dir.push(chars.next()?),
            _ => dir.push(ch),
        }
    }
    None
}

fn measure_once(target: &str, pass: &str, vespera_dir: Option<&CacheDir>) -> Option<f64> {
    // Force a full re-expansion of the fixture lib (deps stay built).
    let _ = cargo().args(["clean", "-p", target]).status();
    // `cargo clean -p` keeps `<target>/vespera/`: its route cache would turn
    // every measured run into a cache hit that skips route collection,
    // validation and OpenAPI generation (the code macro changes touch).
    for dir in vespera_dir.map_or_else(Vec::new, CacheDir::dirs) {
        let _ = fs::remove_dir_all(dir);
    }
    let out = cargo()
        .args([
            "rustc",
            "--quiet",
            "-p",
            target,
            "--lib",
            "--",
            "-Z",
            "time-passes",
        ])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    // A failed build still prints the passes it reached (macro expansion
    // included): its timing must never become a sample or a baseline.
    if !out.status.success() {
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        fatal(&format!(
            "`cargo rustc -p {target}` failed ({}):\n{}",
            out.status,
            tail.join("\n")
        ));
    }
    extract_pass_time(&stderr, pass)
}

/// The timings a comparison can use: finite and positive, sorted with
/// `total_cmp` (a NaN or infinite value from a malformed line is dropped, it
/// cannot abort the run).
fn usable_timings(values: impl IntoIterator<Item = f64>) -> Vec<f64> {
    let mut values: Vec<f64> = values
        .into_iter()
        .filter(|t| t.is_finite() && *t > 0.0)
        .collect();
    values.sort_by(f64::total_cmp);
    values
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        f64::midpoint(sorted[n / 2 - 1], sorted[n / 2])
    }
}

// Sample counts are tiny (a handful of runs), so `usize -> f64` is exact.
#[allow(clippy::cast_precision_loss)]
fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

#[allow(clippy::cast_precision_loss)]
fn stddev(v: &[f64], m: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

fn baselines_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baselines")
}

fn main() {
    let args = parse_args();

    eprintln!("[warm] building `{}` (and deps) once...", args.target);
    let warm = cargo()
        .args(["build", "--quiet", "-p", &args.target, "--lib"])
        .status();
    if !matches!(warm, Ok(s) if s.success()) {
        fatal(&format!("warm build of `{}` failed", args.target));
    }

    let vespera_dir = vespera_build_dir();
    eprintln!(
        "[measure] {} runs of `{}` on `{}`",
        args.runs, args.pass, args.target
    );
    let mut samples = Vec::new();
    for i in 0..args.runs {
        match measure_once(&args.target, &args.pass, vespera_dir.as_ref()) {
            Some(t) => {
                eprintln!("  run {:>2}: {t:.4}s", i + 1);
                samples.push(t);
            }
            None => eprintln!(
                "  run {:>2}: pass `{}` not found in output",
                i + 1,
                args.pass
            ),
        }
    }
    let samples = usable_timings(samples);
    if samples.is_empty() {
        fatal("no samples collected (is the target a lib that uses vespera macros?)");
    }

    let med0 = median(&samples);
    let clean: Vec<f64> = samples
        .iter()
        .copied()
        .filter(|&t| t <= med0 * 3.0)
        .collect();
    let clean = if clean.is_empty() {
        samples.clone()
    } else {
        clean
    };

    let min = clean[0];
    let med = median(&clean);
    let mn = mean(&clean);
    let sd = stddev(&clean, mn);
    let rel_sd = if mn > 0.0 { 100.0 * sd / mn } else { 0.0 };

    println!();
    println!(
        "== {} on `{}` ({} clean / {} total runs) ==",
        args.pass,
        args.target,
        clean.len(),
        samples.len()
    );
    println!("   min={min:.4}s  median={med:.4}s  mean={mn:.4}s  sd={sd:.4}s ({rel_sd:.1}%)");

    if let Some(name) = &args.save_baseline {
        let dir = baselines_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.txt"));
        let body = clean.iter().fold(String::new(), |mut body, t| {
            let _ = writeln!(body, "{t}");
            body
        });
        match fs::write(&path, body) {
            Ok(()) => println!("   saved baseline `{name}` -> {}", path.display()),
            Err(e) => eprintln!("   failed to save baseline `{name}`: {e}"),
        }
    }

    if let Some(name) = &args.baseline {
        let path = baselines_dir().join(format!("{name}.txt"));
        match fs::read_to_string(&path) {
            Ok(s) => {
                let base = usable_timings(s.lines().filter_map(|l| l.trim().parse().ok()));
                if base.is_empty() {
                    eprintln!("   baseline `{name}` has no usable timings");
                } else {
                    let bmin = base[0];
                    let bmed = median(&base);
                    let d_min = 100.0 * (min - bmin) / bmin;
                    let d_med = 100.0 * (med - bmed) / bmed;
                    // Noise-aware verdict on `min` (the robust estimator):
                    // require the change to exceed run-to-run noise (>= 2%).
                    let noise = rel_sd.max(2.0);
                    let verdict = if d_min.abs() <= noise {
                        "NO CHANGE (within noise)"
                    } else if d_min < 0.0 {
                        "IMPROVED"
                    } else {
                        "REGRESSED"
                    };
                    println!();
                    println!("== vs baseline `{name}` ==");
                    println!("   min:    {bmin:.4}s -> {min:.4}s  ({d_min:+.1}%)");
                    println!("   median: {bmed:.4}s -> {med:.4}s  ({d_med:+.1}%)");
                    println!("   verdict: {verdict}  (noise ~{noise:.1}%)");
                }
            }
            Err(e) => eprintln!("   baseline `{name}` not found ({e}); use --save-baseline first"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_path_reads_and_unescapes_the_metadata_field() {
        let json = r#"{"packages":[],"target_directory":"C:\\work\\tar\"get","version":1}"#;
        assert_eq!(
            metadata_path(json, "target_directory"),
            Some(PathBuf::from(r#"C:\work\tar"get"#))
        );
        assert_eq!(
            metadata_path(r#"{"target_directory":"unterminated"#, "target_directory"),
            None
        );
        assert_eq!(metadata_path("{}", "target_directory"), None);
    }

    /// A relative `VESPERA_TARGET_DIR` is resolved against the workspace
    /// root (as the macro resolves it), never the runner's working directory.
    #[test]
    fn build_dir_resolves_the_override_like_the_macro() {
        let json =
            || Some(r#"{"target_directory":"/ws/target","workspace_root":"/ws"}"#.to_owned());
        assert_eq!(
            build_dir_from(Some(PathBuf::from("tgt")), json),
            Some(CacheDir::Exact(
                PathBuf::from("/ws").join("tgt").join("vespera")
            ))
        );
        let absolute = env::temp_dir().join("tgt");
        assert_eq!(
            build_dir_from(Some(absolute.clone()), || None),
            Some(CacheDir::Exact(absolute.join("vespera")))
        );
        assert_eq!(
            build_dir_from(None, json),
            Some(CacheDir::UnderTarget(PathBuf::from("/ws/target")))
        );
        assert_eq!(build_dir_from(None, || None), None);
    }

    /// With a target triple the macro caches under `<target>/<triple>/vespera`:
    /// that directory is cleared too, so no measured run is a cache hit.
    #[test]
    fn cache_dirs_include_the_target_triple_directory() {
        let target = env::temp_dir().join(format!("vespera-bench-{}", std::process::id()));
        let _ = fs::remove_dir_all(&target);
        let triple = target.join("x86_64-unknown-linux-gnu");
        fs::create_dir_all(triple.join("vespera")).unwrap();
        fs::create_dir_all(triple.join("debug")).unwrap();
        fs::create_dir_all(target.join("debug")).unwrap();
        assert_eq!(
            CacheDir::UnderTarget(target.clone()).dirs(),
            [target.join("vespera"), triple.join("vespera")]
        );
        // `cargo doc` output is not a triple directory: its `vespera` (the
        // rustdoc output of this crate) must survive a measured run.
        fs::create_dir_all(target.join("doc").join("vespera")).unwrap();
        assert_eq!(
            CacheDir::UnderTarget(target.clone()).dirs(),
            [target.join("vespera"), triple.join("vespera")]
        );
        let exact = target.join("custom");
        assert_eq!(CacheDir::Exact(exact.clone()).dirs(), [exact]);
        fs::remove_dir_all(&target).unwrap();
        assert_eq!(
            CacheDir::UnderTarget(target.clone()).dirs(),
            [target.join("vespera")]
        );
    }

    /// A NaN or infinite timing (a malformed line) is dropped rather than
    /// panicking the sort; the rest come back ascending.
    #[test]
    fn usable_timings_drops_non_finite_and_non_positive_values() {
        let timings = usable_timings([2.5, f64::NAN, 0.0, -1.0, f64::INFINITY, 0.5, 1.0]);
        assert_eq!(timings, [0.5, 1.0, 2.5]);
        assert_eq!(usable_timings([f64::NAN]), Vec::<f64>::new());
    }
}
