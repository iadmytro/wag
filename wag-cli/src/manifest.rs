//! Package manifest: `wag.toml` parsing and package-root discovery.
//!
//! A `wag.toml` file marks the root of a WAG package. It declares the package
//! name and its path dependencies on other packages.
//!
//! ```toml
//! name = "mypackage"
//!
//! [deps]
//! utils = "../utils"
//! math = "../math"
//! ```
//!
//! Each key in `[deps]` is the dependency package name; the value is a
//! relative path (from this package's root) to the dependency's root
//! directory (which must contain its own `wag.toml`).
//!
//! Design: `doc/wag-modules.md` §10 (v1 scope → Phase 2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Diagnostic, PipelineStage};

/// A parsed `wag.toml` package manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Package name (used in diagnostics and the module namespace).
    pub name: String,
    /// Path dependencies: package name → relative path to the dep's root dir.
    pub deps: BTreeMap<String, PathBuf>,
}

impl Manifest {
    /// Resolve a dependency's absolute root path relative to `self_root`.
    pub fn dep_root(&self, self_root: &Path, dep_name: &str) -> Option<PathBuf> {
        self.deps
            .get(dep_name)
            .map(|rel| self_root.join(rel))
    }
}

/// Load and parse `wag.toml` from `root`. Returns an error if the file is
/// missing, unreadable, or malformed.
pub fn load_manifest(root: &Path) -> Result<Manifest, Diagnostic> {
    let path = root.join("wag.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| Diagnostic {
        stage: PipelineStage::Resolve,
        span: wag_syntax::Span::new(0, 0),
        message: format!("cannot read manifest `{}`: {e}", path.display()),
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;

    parse_manifest(&text, &path)
}

/// Parse manifest text into a `Manifest`. Separated from `load_manifest` for
/// testability without filesystem I/O.
fn parse_manifest(text: &str, path: &Path) -> Result<Manifest, Diagnostic> {
    let value: toml::Value = toml::from_str(text).map_err(|e| Diagnostic {
        stage: PipelineStage::Resolve,
        span: wag_syntax::Span::new(0, 0),
        message: format!("malformed manifest `{}`: {e}", path.display()),
        hint: Some(
            "Expected: name = \"...\" and [deps] section with key = \"relative/path\" entries."
                .into(),
        ),
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;

    let table = match &value {
        toml::Value::Table(t) => t,
        _ => {
            return Err(Diagnostic {
                stage: PipelineStage::Resolve,
                span: wag_syntax::Span::new(0, 0),
                message: format!(
                    "manifest `{}` must be a TOML table",
                    path.display()
                ),
                hint: None,
                rule: None,
                type_stage: None,
                module_name: None,
                origin_span: None,
            })
        }
    };

    // `name` (required)
    let name = table
        .get("name")
        .and_then(|v| match v {
            toml::Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or_else(|| Diagnostic {
            stage: PipelineStage::Resolve,
            span: wag_syntax::Span::new(0, 0),
            message: format!("manifest `{}` missing required `name` field", path.display()),
            hint: Some("Add: name = \"mypackage\"".into()),
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        })?;

    // `[deps]` (optional)
    let mut deps = BTreeMap::new();
    if let Some(toml::Value::Table(deps_table)) = table.get("deps") {
        for (key, val) in deps_table {
            match val {
                toml::Value::String(s) => {
                    deps.insert(key.clone(), PathBuf::from(s));
                }
                other => {
                    return Err(Diagnostic {
                        stage: PipelineStage::Resolve,
                        span: wag_syntax::Span::new(0, 0),
                        message: format!(
                            "manifest `{}`: dep `{key}` must be a string path, got {other:?}",
                            path.display()
                        ),
                        hint: Some(format!(
                            "Expected: {key} = \"../relative/path\""
                        )),
                        rule: None,
                        type_stage: None,
                        module_name: None,
                        origin_span: None,
                    })
                }
            }
        }
    }

    Ok(Manifest { name, deps })
}

/// Walk up from `entry`'s parent directory looking for a `wag.toml` file.
/// Returns the directory containing it, or the entry's parent directory if
/// no manifest is found (Phase 1 single-directory behavior).
pub fn find_package_root(entry: &Path) -> PathBuf {
    let original = entry
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut dir = original.clone();
    loop {
        if dir.join("wag.toml").exists() {
            return dir;
        }
        match dir.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                dir = parent.to_path_buf();
            }
            _ => return original,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let m = parse_manifest("name = \"foo\"\n", Path::new("wag.toml")).unwrap();
        assert_eq!(m.name, "foo");
        assert!(m.deps.is_empty());
    }

    #[test]
    fn parse_with_deps() {
        let text = r#"
name = "app"

[deps]
utils = "../utils"
math = "../libs/math"
"#;
        let m = parse_manifest(text, Path::new("wag.toml")).unwrap();
        assert_eq!(m.name, "app");
        assert_eq!(m.deps.len(), 2);
        assert_eq!(m.deps["utils"], PathBuf::from("../utils"));
        assert_eq!(m.deps["math"], PathBuf::from("../libs/math"));
    }

    #[test]
    fn parse_missing_name() {
        let err = parse_manifest("[deps]\nfoo = \"../foo\"\n", Path::new("wag.toml")).unwrap_err();
        assert!(err.message.contains("missing required `name`"));
    }

    #[test]
    fn parse_bad_dep_type() {
        let err = parse_manifest(
            "name = \"app\"\n[deps]\nfoo = 42\n",
            Path::new("wag.toml"),
        )
        .unwrap_err();
        assert!(err.message.contains("must be a string path"));
    }

    #[test]
    fn parse_malformed_toml() {
        let err = parse_manifest("name = ", Path::new("wag.toml")).unwrap_err();
        assert!(err.message.contains("malformed manifest"));
    }

    #[test]
    fn find_root_no_manifest() {
        let tmp = std::env::temp_dir().join("wag_test_no_manifest");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let entry = tmp.join("main.wag");
        std::fs::write(&entry, "").unwrap();
        let root = find_package_root(&entry);
        // No wag.toml anywhere → returns the entry's parent dir.
        assert_eq!(root, tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_root_with_manifest() {
        let tmp = std::env::temp_dir().join("wag_test_manifest");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("wag.toml"), "name = \"test\"\n").unwrap();
        let entry = tmp.join("main.wag");
        std::fs::write(&entry, "").unwrap();
        let root = find_package_root(&entry);
        assert_eq!(root, tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_root_nested() {
        let tmp = std::env::temp_dir().join("wag_test_nested");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/sub")).unwrap();
        std::fs::write(tmp.join("wag.toml"), "name = \"test\"\n").unwrap();
        let entry = tmp.join("src/sub/main.wag");
        std::fs::write(&entry, "").unwrap();
        let root = find_package_root(&entry);
        assert_eq!(root, tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
