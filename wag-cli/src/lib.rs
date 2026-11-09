//! `wagc` library surface — full pipeline for black-box tests and the CLI binary.
//!
//! Pipeline order (spec §4.6.7):
//! `resolve → wag-syntax → wag-types → wag-check (pass 1+2) → wag-interp load → wag-runtime execute`
//! (halt at the first rejecting stage).

pub mod bind;
pub mod manifest;
pub mod resolve;

mod diagnostics;
mod pipeline;

pub use diagnostics::{render_diagnostic, Diagnostic, PipelineStage};
pub use pipeline::{
    call_fn, compile_and_load, compile_multi_module, compile_source, mirror_src_path,
    read_stdin_file, run_entry_path, run_multi_module, run_path, run_source, run_test_file,
    workspace_fixtures_dir, Compiled, PipelineOk, RunOptions,
    RunResult, TestOutcome,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Prepend an explicit prelude import (D053) so inline `@std` programs
    /// (D043) resolve their names. The built-in prelude is located via
    /// `resolve::builtin_prelude_path` (CWD, then this crate's repo copy).
    fn with_prelude(user: &str) -> String {
        format!("import\n  prelude\n    ..\n{user}")
    }

    /// `@main` writes `"hi"` exactly once; the captured stdout must hold the
    /// payload exactly once (regression: the runtime used to mirror every
    /// `Write` to process stdout on top of the capture buffer, so the CLI
    /// emitted each `Write` twice).
    #[test]
    fn run_source_captures_write_once() {
        let src = with_prelude(
            r#"
MainMsg = Start (List String) | End Int
@main = @std: Int, MainMsg -> Int: st, msg -> match msg
  Start _ ->
    s@ = @std
    s@ (Write "hi")
    self@ (End 0)
    0
  End code -> code
"#,);
        let mut opts = RunOptions {
            timeout: Duration::from_secs(10),
            ..Default::default()
        };
        opts.emit_stdout = true;
        match run_source(&src, opts).unwrap() {
            PipelineOk::Executed(r) => {
                assert_eq!(r.stdout, "hi", "captured stdout must be the payload exactly once");
                r.runtime.shutdown();
            }
            PipelineOk::Checked(_) => panic!("expected Executed"),
        }
    }

    /// `emit_stdout: false` (harness mode) must not alter the captured buffer.
    #[test]
    fn run_source_capture_independent_of_emit() {
        let src = with_prelude(
            r#"
MainMsg = Start (List String) | End Int
@main = @std: Int, MainMsg -> Int: st, msg -> match msg
  Start _ ->
    s@ = @std
    s@ (Write "Hello")
    self@ (End 0)
    0
  End code -> code
"#,);
        let opts = RunOptions {
            timeout: Duration::from_secs(10),
            emit_stdout: false,
            ..Default::default()
        };
        match run_source(&src, opts).unwrap() {
            PipelineOk::Executed(r) => {
                assert_eq!(r.stdout, "Hello");
                r.runtime.shutdown();
            }
            PipelineOk::Checked(_) => panic!("expected Executed"),
        }
    }

    /// `@main` echoes its first argv element byte-for-byte via `@std`; the `End`
    /// arm exits with the code. Verifies argv reaches `@main` as a `List String`
    /// (the `Start` message). Empty argv → no write.
    #[test]
    fn argv_reaches_main_as_nested_list() {
        let src = with_prelude(
            r#"
MainMsg = Start (List String) | End Int
@main = @std: Int, MainMsg -> Int: st, msg -> match msg
  Start argv -> match argv.peek
    None ->
      self@ (End 0)
      0
    Some first ->
      s@ = @std
      s@ (Write first)
      self@ (End 0)
      0
  End code -> code
"#,);
        let mut opts = RunOptions {
            timeout: Duration::from_secs(10),
            ..Default::default()
        };
        opts.argv = vec!["abc".into(), "de".into()];
        match run_source(&src, opts.clone()).unwrap() {
            PipelineOk::Executed(r) => {
                assert_eq!(r.stdout, "abc", "first argv element must be echoed");
                r.runtime.shutdown();
            }
            PipelineOk::Checked(_) => panic!("expected Executed"),
        }
        // Empty argv → None peek → no write.
        opts.argv.clear();
        match run_source(&src, opts).unwrap() {
            PipelineOk::Executed(r) => assert_eq!(r.stdout, ""),
            PipelineOk::Checked(_) => panic!("expected Executed"),
        }
    }

    /// `..`-importing the prelude must bind `@std` to its *handle* type
    /// (`Actor StdMsg`), not the actor's behavior function type.
    /// Regression: `..` expansion used the bare name as the import source key,
    /// which resolved to the behavior scheme registered under `("std", arity)`.
    #[test]
    fn dotdot_imported_actor_handle_is_actor_type() {
        let src = with_prelude("x = 1\n");
        let checked = compile_source(&src).unwrap();
        match checked.typed().actors.get("std") {
            Some(wag_types::Ty::App(head, args)) => {
                assert_eq!(head.as_str(), "Actor", "`@std` must be an Actor handle");
                assert_eq!(args.len(), 1);
            }
            other => panic!("`@std` should be `Actor StdMsg`, got {other:?}"),
        }
    }

    /// A func `std` and the prelude's actor `@std` may coexist (§2: `@name` and
    /// `name` are distinct atoms). The bare reference must resolve to the function.
    #[test]
    fn func_and_actor_share_bare_name() {
        let src = with_prelude(
            r#"
MainMsg = Start (List (List Int))
std = 41
useStd = std + 1
@main = @std: Option Int, MainMsg -> Option Int: st, msg -> match msg
  Start _ ->
    s@ = @std
    s@ (Write "ok")
    None
  Line _ -> st
  EndOfInput -> None
"#,
        );
        let checked = compile_source(&src).unwrap();
        assert_eq!(
            checked.typed().bindings.get(&("std".to_string(), 0)),
            Some(&wag_types::Ty::int()),
            "bare `std` must be the Int function, not the actor behavior"
        );
    }

    /// Rule 10 (use-after-move) applies inside actor bodies too.
    /// Regression: wag-check looked up actor behaviors under the handle key
    /// `("@name", 0)` expecting a Fun type — since D052 that key holds
    /// `Actor Msg`, so actor bodies were silently skipped by the linearity pass.
    #[test]
    fn actor_body_use_after_move_rejected() {
        let src = with_prelude(
            r#"
MainMsg = Probe (Option (List Int))
@main = @std: Option Int, MainMsg -> Option Int: st, msg -> match msg
  Probe o -> match o
    None -> st
    Some xs -> match xs
      LH _ _ _ _ -> match xs
        LH _ _ _ _ -> st
  Line _ -> st
  EndOfInput -> None
"#,
        );
        let d = compile_source(&src).unwrap_err();
        assert_eq!(d.rule, Some(wag_check::Rule::Linearity), "expected rule 10, got: {d:?}");
    }

    #[test]
    fn read_stdin_file_trailing_newline_handling() {
        let p = std::env::temp_dir().join("wag_cli_stdin_test.txt");
        // Normal file ending in a single newline → no phantom empty line.
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(read_stdin_file(&p).unwrap(), vec!["a", "b", "c"]);
        // No trailing newline → same lines.
        std::fs::write(&p, "a\nb\nc").unwrap();
        assert_eq!(read_stdin_file(&p).unwrap(), vec!["a", "b", "c"]);
        // Trailing empty line is dropped (documented contract).
        std::fs::write(&p, "a\nb\n\n").unwrap();
        assert_eq!(read_stdin_file(&p).unwrap(), vec!["a", "b"]);
        let _ = std::fs::remove_file(&p);
    }
}
