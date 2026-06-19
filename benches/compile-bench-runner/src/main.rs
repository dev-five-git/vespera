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
//! Methodology: each iteration runs `cargo clean -p <target>` to force a full
//! re-expansion, then `cargo rustc … -- -Z time-passes` and parses the pass
//! time. Compile time has only *positive* noise (a busy machine only ever
//! adds time), so `min` is the robust point estimate; `median`/`mean`/`sd`
//! are reported too. Gross outliers (> 3x median, e.g. AV/FS hiccups) are
//! dropped before stats.

use std::env;
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
        let mut next = |flag: &str| it.next().unwrap_or_else(|| fatal(&format!("{flag} needs a value")));
        match arg.as_str() {
            "--target" => a.target = next("--target"),
            "--pass" => a.pass = next("--pass"),
            "--runs" => {
                a.runs = next("--runs").parse().unwrap_or_else(|_| fatal("--runs must be an integer"));
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

fn measure_once(target: &str, pass: &str) -> Option<f64> {
    // Force a full re-expansion of the fixture lib (deps stay built).
    let _ = cargo().args(["clean", "-p", target]).status();
    let out = cargo()
        .args(["rustc", "--quiet", "-p", target, "--lib", "--", "-Z", "time-passes"])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    extract_pass_time(&stderr, pass)
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

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

    eprintln!(
        "[measure] {} runs of `{}` on `{}`",
        args.runs, args.pass, args.target
    );
    let mut samples = Vec::new();
    for i in 0..args.runs {
        match measure_once(&args.target, &args.pass) {
            Some(t) => {
                eprintln!("  run {:>2}: {t:.4}s", i + 1);
                samples.push(t);
            }
            None => eprintln!("  run {:>2}: pass `{}` not found in output", i + 1, args.pass),
        }
    }
    if samples.is_empty() {
        fatal("no samples collected (is the target a lib that uses vespera macros?)");
    }

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med0 = median(&samples);
    let clean: Vec<f64> = samples.iter().copied().filter(|&t| t <= med0 * 3.0).collect();
    let clean = if clean.is_empty() { samples.clone() } else { clean };

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
        let body: String = clean.iter().map(|t| format!("{t}\n")).collect();
        match fs::write(&path, body) {
            Ok(()) => println!("   saved baseline `{name}` -> {}", path.display()),
            Err(e) => eprintln!("   failed to save baseline `{name}`: {e}"),
        }
    }

    if let Some(name) = &args.baseline {
        let path = baselines_dir().join(format!("{name}.txt"));
        match fs::read_to_string(&path) {
            Ok(s) => {
                let mut base: Vec<f64> =
                    s.lines().filter_map(|l| l.trim().parse().ok()).collect();
                if base.is_empty() {
                    eprintln!("   baseline `{name}` is empty");
                } else {
                    base.sort_by(|a, b| a.partial_cmp(b).unwrap());
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
