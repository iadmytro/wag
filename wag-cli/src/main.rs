//! `wagc` — WAG compiler / runner (Milestone 10).
//!
//! ```text
//! wagc run <file.wag> [--stdin-file PATH] [-- arg...]
//! wagc test <test-dir>
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use wag_cli::{
    read_stdin_file, render_diagnostic, run_entry_path, run_test_file, PipelineOk, RunOptions,
    TestOutcome,
};
use wag_runtime::{SchedulerConfig, MAX_EXEC_TIMEOUT};

fn usage() -> ! {
    eprintln!(
        "\
wagc — WAG compiler/runner

Usage:
  wagc run <file.wag> [options] [-- <argv>...]
  wagc test <test-dir>

Commands:
  run   Compile and run a program (parse → type → check → load → run @main if present)
  test  Run function unit tests: for each `<test-dir>/<rel>.wag`, merge it into the
        mirrored `src/<rel>.wag` module and call its `test_main`. A failing `assert`
        marks that test FAILED.

Options (run):
  --stdin-file <path>   Feed @std from this file (one line per newline), then EOF
  --timeout <secs>      Max seconds to wait for @main (default: 10; hard max 10)
  --threads <n>         Scheduler worker threads (default: 0 = single-threaded)
  -h, --help            Show this help
"
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }

    let cmd = args.remove(0);
    if cmd == "test" {
        return cmd_test(args);
    }
    if cmd != "run" {
        eprintln!("error: unknown command `{cmd}` (try `wagc run <file.wag>` or `wagc test <dir>`)");
        usage();
    }

    let mut file: Option<PathBuf> = None;
    let mut stdin_file: Option<PathBuf> = None;
    let mut timeout_secs: u64 = 10; // hard dev-mode ceiling (CLAUDE.md); overrides capped below
    let mut threads: usize = 0;
    let mut program_argv: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                program_argv.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--stdin-file" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --stdin-file needs a path");
                    return ExitCode::from(2);
                }
                stdin_file = Some(PathBuf::from(&args[i]));
            }
            "--timeout" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --timeout needs a number");
                    return ExitCode::from(2);
                }
                match args[i].parse::<u64>() {
                    Ok(n) => timeout_secs = n,
                    Err(_) => {
                        eprintln!("error: invalid --timeout value");
                        return ExitCode::from(2);
                    }
                }
            }
            "--threads" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --threads needs a number");
                    return ExitCode::from(2);
                }
                match args[i].parse::<usize>() {
                    Ok(n) => threads = n,
                    Err(_) => {
                        eprintln!("error: invalid --threads value");
                        return ExitCode::from(2);
                    }
                }
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option `{s}`");
                return ExitCode::from(2);
            }
            s => {
                if file.is_some() {
                    eprintln!("error: unexpected argument `{s}`");
                    return ExitCode::from(2);
                }
                file = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }

    let Some(path) = file else {
        eprintln!("error: missing <file.wag>");
        return ExitCode::from(2);
    };

    let stdin_lines = match stdin_file {
        Some(p) => match read_stdin_file(&p) {
            Ok(lines) => Some(lines),
            Err(e) => {
                eprintln!("error: cannot read --stdin-file: {e}");
                return ExitCode::from(2);
            }
        },
        None => None,
    };

    // Enforce the hard 10s ceiling (CLAUDE.md) with a helpful note.
    let max_secs = MAX_EXEC_TIMEOUT.as_secs();
    if timeout_secs > max_secs {
        eprintln!("note: --timeout capped at {max_secs}s (hard dev-mode limit)");
        timeout_secs = max_secs;
    }

    let opts = RunOptions {
        argv: program_argv,
        stdin_lines,
        timeout: Duration::from_secs(timeout_secs),
        scheduler: SchedulerConfig { workers: threads },
        emit_stdout: true,
        print_stats: true, // dev-mode: always print steps + allocs for analysis
    };

    match run_entry_path(&path, opts) {
        Ok(PipelineOk::Checked(_)) => {
            eprintln!(
                "wagc: {} — ok (checked + loaded; no @main to execute)",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(PipelineOk::Executed(r)) => {
            // stdout already printed from captured buffer inside run_source
            let _ = r;
            ExitCode::SUCCESS
        }
        Err((src, diag)) => {
            // Name the file after the module that owns the error so the header
            // (`name:line:col`) matches the source we rendered against.
            let name: String = diag.module_name.clone().unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("input.wag")
                    .to_string()
            });
            let rendered = render_diagnostic(&name, &src, &diag);
            eprint!("{rendered}");
            if rendered.trim().is_empty() {
                eprintln!("{diag}");
            }
            ExitCode::from(1)
        }
    }
}

/// `wagc test <test-dir>`: discover mirrored tests, run each, print a summary.
fn cmd_test(args: Vec<String>) -> ExitCode {
    let mut test_dir: Option<PathBuf> = None;
    for a in &args {
        match a.as_str() {
            "-h" | "--help" => usage(),
            s if s.starts_with('-') => {
                eprintln!("error: unknown option `{s}` (test takes only <test-dir>)");
                return ExitCode::from(2);
            }
            s => {
                if test_dir.is_some() {
                    eprintln!("error: unexpected argument `{s}`");
                    return ExitCode::from(2);
                }
                test_dir = Some(PathBuf::from(s));
            }
        }
    }
    let dir = match test_dir {
        Some(d) => d,
        None => {
            eprintln!("error: missing <test-dir> (try `wagc test examples/parser/test`)");
            return ExitCode::from(2);
        }
    };

    let files = discover_test_files(&dir);
    if files.is_empty() {
        eprintln!("no test files (*.wag) found under {}", dir.display());
        return ExitCode::from(2);
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = 0usize;
    for f in &files {
        match run_test_file(f) {
            Ok(TestOutcome::Pass) => {
                println!("ok   {}", rel(&dir, f));
                passed += 1;
            }
            Ok(TestOutcome::Fail(msg)) => {
                println!("FAIL {}", rel(&dir, f));
                println!("     {msg}");
                failed += 1;
            }
            Err(diag) => {
                println!("ERR  {}", rel(&dir, f));
                // Best-effort: render against the test file's own source so a
                // parse/type error in the test is highlighted; fall back to the
                // plain message if the file can't be read.
                match std::fs::read_to_string(f) {
                    Ok(src) => {
                        let name = f.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("test.wag");
                        eprint!("{}", render_diagnostic(name, &src, &diag));
                    }
                    Err(_) => eprintln!("     {}", diag.message),
                }
                errors += 1;
            }
        }
    }

    let total = passed + failed + errors;
    println!();
    println!("{passed} passed, {failed} failed, {errors} error(s) ({total} total)");
    if failed == 0 && errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Recursively collect `*.wag` files under `dir`, sorted for deterministic order.
fn discover_test_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_wag(dir, &mut out);
    out.sort();
    out
}

fn walk_wag(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_wag(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("wag") {
            out.push(p);
        }
    }
}

/// Display `p` relative to `base` when possible (clean per-test labels).
fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.display().to_string())
}
