//! Record-update desugaring template (§3.1.2).
//!
//! Type-directed path resolution lives in `wag-check`. This module only
//! builds the nested-`match`-and-rebuild AST once each path step has already
//! been resolved to `(constructor, field_index, arity)`.

use crate::ast::*;
use crate::Span;

/// One resolved step along a field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFieldStep {
    /// Constructor name matched at this level (single-constructor type).
    pub constructor: String,
    /// Total number of fields on that constructor.
    pub arity: usize,
    /// 0-based index of the field selected or updated at this step.
    pub field_index: usize,
}

/// One leaf assignment after resolution: full path of steps + RHS expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLeaf {
    pub steps: Vec<ResolvedFieldStep>,
    pub value: Expr,
}

/// Build the §3.1.2 desugared form:
/// 1. `let v_i = rhs_i in ...` for every leaf, in clause order;
/// 2. nested `match` shared by common path prefixes;
/// 3. constructor rebuild carrying untouched fields through.
///
/// `leaves` must be non-empty. Empty `steps` on a leaf is rejected.
pub fn desugar_record_update_template(
    receiver: Expr,
    leaves: &[ResolvedLeaf],
    span: Span,
) -> Result<Expr, String> {
    if leaves.is_empty() {
        return Err("record update requires at least one leaf".into());
    }
    for (i, leaf) in leaves.iter().enumerate() {
        if leaf.steps.is_empty() {
            return Err(format!("leaf {i} has empty resolved path"));
        }
    }

    // Fresh names for pre-evaluated RHS values and match binders.
    let mut counter = 0usize;
    let mut fresh = |prefix: &str| -> String {
        let n = counter;
        counter += 1;
        format!("__{prefix}{n}")
    };

    let mut rhs_names = Vec::with_capacity(leaves.len());
    let mut lets: Vec<(String, Expr)> = Vec::with_capacity(leaves.len());
    for leaf in leaves {
        let name = fresh("v");
        rhs_names.push(name.clone());
        lets.push((name, leaf.value.clone()));
    }

    // Build a trie of updates keyed by successive ResolvedFieldStep.
    #[derive(Default)]
    struct Node {
        /// Children keyed by (constructor, arity, field_index).
        children: Vec<(ResolvedFieldStep, Node)>,
        /// If Some(i), this node is a leaf referring to rhs_names[i].
        leaf_rhs: Option<usize>,
    }

    fn insert(node: &mut Node, steps: &[ResolvedFieldStep], rhs_idx: usize) -> Result<(), String> {
        if steps.is_empty() {
            if node.leaf_rhs.is_some() || !node.children.is_empty() {
                return Err("overlapping record-update paths".into());
            }
            node.leaf_rhs = Some(rhs_idx);
            return Ok(());
        }
        if node.leaf_rhs.is_some() {
            return Err("overlapping record-update paths".into());
        }
        let head = &steps[0];
        let pos = node
            .children
            .iter()
            .position(|(s, _)| {
                s.constructor == head.constructor
                    && s.arity == head.arity
                    && s.field_index == head.field_index
            });
        match pos {
            Some(i) => insert(&mut node.children[i].1, &steps[1..], rhs_idx),
            None => {
                let mut child = Node::default();
                insert(&mut child, &steps[1..], rhs_idx)?;
                node.children.push((head.clone(), child));
                Ok(())
            }
        }
    }

    let mut root = Node::default();
    for (i, leaf) in leaves.iter().enumerate() {
        insert(&mut root, &leaf.steps, i)?;
    }

    // Group root children that share the same constructor/arity (one match).
    fn rebuild_level(
        scrutinee: Expr,
        siblings: &[(ResolvedFieldStep, Node)],
        rhs_names: &[String],
        fresh: &mut dyn FnMut(&str) -> String,
        span: Span,
    ) -> Expr {
        if siblings.is_empty() {
            return scrutinee;
        }
        // All siblings at one match level must share constructor + arity.
        let ctor = siblings[0].0.constructor.clone();
        let arity = siblings[0].0.arity;
        debug_assert!(siblings
            .iter()
            .all(|(s, _)| s.constructor == ctor && s.arity == arity));

        let binders: Vec<String> = (0..arity).map(|_| fresh("f")).collect();
        let binder_pats: Vec<Pat> = binders
            .iter()
            .map(|n| Pat::Var {
                name: n.clone(),
                span,
            })
            .collect();

        // For each field index, either keep the binder or rebuild from child.
        let mut field_exprs: Vec<Expr> = binders
            .iter()
            .map(|n| Expr::Var {
                name: n.clone(),
                span,
            })
            .collect();

        for (step, child) in siblings {
            let idx = step.field_index;
            if let Some(rhs_i) = child.leaf_rhs {
                // Direct leaf at this field.
                field_exprs[idx] = Expr::Var {
                    name: rhs_names[rhs_i].clone(),
                    span,
                };
            } else if !child.children.is_empty() {
                // Nested match on this field binder.
                let nested_scrut = Expr::Var {
                    name: binders[idx].clone(),
                    span,
                };
                field_exprs[idx] =
                    rebuild_level(nested_scrut, &child.children, rhs_names, fresh, span);
            }
        }

        let body = Expr::Apply {
            fun: Box::new(Expr::Upper {
                name: ctor.clone(),
                span,
            }),
            args: field_exprs,
            span,
        };

        // Nullary constructors: Apply with zero args is fine; pattern is bare ctor.
        let pattern = if arity == 0 {
            Pat::Ctor {
                name: ctor,
                qualifier: None,
                args: vec![],
                span,
            }
        } else {
            Pat::Ctor {
                name: ctor,
                qualifier: None,
                args: binder_pats,
                span,
            }
        };

        Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![MatchArm { pattern, body }],
            span,
        }
    }

    let core = rebuild_level(receiver, &root.children, &rhs_names, &mut fresh, span);

    // Wrap lets outermost-first (first clause evaluated first).
    let mut acc = core;
    for (name, val) in lets.into_iter().rev() {
        let s = Span::new(val.span().start, acc.span().end);
        acc = Expr::Bind {
            name,
            value: Box::new(val),
            body: Box::new(acc),
            span: s,
        };
    }
    // Force outer span
    match &mut acc {
        Expr::Bind { span: sp, .. } => *sp = span,
        Expr::Seq { span: sp, .. } => *sp = span,
        Expr::Match { span: sp, .. } => *sp = span,
        _ => {}
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn var(n: &str) -> Expr {
        Expr::Var {
            name: n.into(),
            span: sp(),
        }
    }

    fn int(v: i64) -> Expr {
        Expr::Int {
            value: v,
            span: sp(),
        }
    }

    /// Two-level path like `p.address.city = newCity` on Person → Address.
    #[test]
    fn desugar_two_level_single_leaf_shape() {
        // Person has 2 fields for a tiny stand-in: name, address
        // Address has 2 fields: street, city
        let leaves = vec![ResolvedLeaf {
            steps: vec![
                ResolvedFieldStep {
                    constructor: "Person".into(),
                    arity: 2,
                    field_index: 1,
                },
                ResolvedFieldStep {
                    constructor: "Address".into(),
                    arity: 2,
                    field_index: 1,
                },
            ],
            value: var("newCity"),
        }];
        let out = desugar_record_update_template(var("p"), &leaves, sp()).unwrap();

        // Expect: let __v0 = newCity in match p | Person f0 f1 -> match f1 | Address g0 g1 -> Person f0 (Address g0 __v0)
        match out {
            Expr::Bind { name, value, body, .. } => {
                assert_eq!(name, "__v0");
                assert_eq!(*value, var("newCity"));
                match body.as_ref() {
                    Expr::Match {
                        scrutinee, arms, ..
                    } => {
                        assert_eq!(**scrutinee, var("p"));
                        assert_eq!(arms.len(), 1);
                        match &arms[0].pattern {
                            Pat::Ctor { name, args, .. } => {
                                assert_eq!(name, "Person");
                                assert_eq!(args.len(), 2);
                            }
                            _ => panic!("expected Person ctor pat"),
                        }
                        // body is Apply Person [name, nested]
                        match &arms[0].body {
                            Expr::Apply { fun, args, .. } => {
                                assert!(matches!(fun.as_ref(), Expr::Upper { name, .. } if name == "Person"));
                                assert_eq!(args.len(), 2);
                                // second arg is nested match
                                match &args[1] {
                                    Expr::Match { arms: inner, .. } => {
                                        assert_eq!(inner.len(), 1);
                                        match &inner[0].body {
                                            Expr::Apply { fun, args, .. } => {
                                                assert!(matches!(
                                                    fun.as_ref(),
                                                    Expr::Upper { name, .. } if name == "Address"
                                                ));
                                                assert_eq!(args.len(), 2);
                                                assert!(matches!(
                                                    &args[1],
                                                    Expr::Var { name, .. } if name == "__v0"
                                                ));
                                            }
                                            _ => panic!("expected Address rebuild"),
                                        }
                                    }
                                    _ => panic!("expected nested match on address"),
                                }
                            }
                            _ => panic!("expected Person rebuild"),
                        }
                    }
                    _ => panic!("expected outer match"),
                }
            }
            _ => panic!("expected outer let"),
        }
    }

    #[test]
    fn desugar_sibling_leaves_share_one_match() {
        // with p
        //   .age = a
        //   .score = s
        // Person arity 3: name, age, score — update indices 1 and 2
        let leaves = vec![
            ResolvedLeaf {
                steps: vec![ResolvedFieldStep {
                    constructor: "Person".into(),
                    arity: 3,
                    field_index: 1,
                }],
                value: var("a"),
            },
            ResolvedLeaf {
                steps: vec![ResolvedFieldStep {
                    constructor: "Person".into(),
                    arity: 3,
                    field_index: 2,
                }],
                value: var("s"),
            },
        ];
        let out = desugar_record_update_template(var("p"), &leaves, sp()).unwrap();
        // Two lets, one match
        let mut lets = 0;
        let mut e = &out;
        while let Expr::Bind { body, .. } = e {
            lets += 1;
            e = body.as_ref();
        }
        assert_eq!(lets, 2);
        match e {
            Expr::Match { arms, .. } => {
                assert_eq!(arms.len(), 1);
                match &arms[0].body {
                    Expr::Apply { args, .. } => {
                        assert_eq!(args.len(), 3);
                        assert!(matches!(&args[1], Expr::Var { name, .. } if name == "__v0"));
                        assert!(matches!(&args[2], Expr::Var { name, .. } if name == "__v1"));
                    }
                    _ => panic!(),
                }
            }
            _ => panic!("expected single match"),
        }
    }

    #[test]
    fn desugar_rejects_empty_leaves() {
        assert!(desugar_record_update_template(var("p"), &[], sp()).is_err());
    }
}
