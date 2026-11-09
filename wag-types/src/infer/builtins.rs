//! Built-in type constructor registration.

use crate::types::{CtorInfo, FieldInfo, Scheme, Ty};

use super::context::Context;

impl Context {
    pub(super) fn install_builtins(&mut self) {
        // Nullary / data constructors with polymorphic schemes.
        // True, False : Bool
        self.add_builtin_ctor(
            "True",
            "Bool",
            vec![],
            Scheme {
                vars: vec![],
                body: Ty::bool(),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "False",
            "Bool",
            vec![],
            Scheme {
                vars: vec![],
                body: Ty::bool(),
            constraints: vec![],            },
        );
        // Unit
        self.add_builtin_ctor(
            "Unit",
            "Unit",
            vec![],
            Scheme {
                vars: vec![],
                body: Ty::unit(),
            constraints: vec![],            },
        );
        // Pair : a, b -> Pair a b
        {
            let a = 0;
            let b = 1;
            self.add_builtin_ctor(
                "Pair",
                "Pair",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::Bound(a),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::Bound(b),
                    },
                ],
                Scheme {
                    vars: vec![a, b],
                    body: Ty::Fun(
                        vec![Ty::Bound(a), Ty::Bound(b)],
                        Box::new(Ty::pair(Ty::Bound(a), Ty::Bound(b))),
                    ),
                constraints: vec![],                },
            );
        }
        // Triple
        {
            let a = 0;
            let b = 1;
            let c = 2;
            self.add_builtin_ctor(
                "Triple",
                "Triple",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::Bound(a),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::Bound(b),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::Bound(c),
                    },
                ],
                Scheme {
                    vars: vec![a, b, c],
                    body: Ty::Fun(
                        vec![Ty::Bound(a), Ty::Bound(b), Ty::Bound(c)],
                        Box::new(Ty::triple(Ty::Bound(a), Ty::Bound(b), Ty::Bound(c))),
                    ),
                constraints: vec![],                },
            );
        }

        // StdMsg / NetMsg built-in constructors (§5.5.7).
        // RequestLine : Actor reply -> StdMsg  (reply type flexible via fresh var each use)
        {
            let r = 0;
            self.add_builtin_ctor(
                "RequestLine",
                "StdMsg",
                vec![FieldInfo {
                    label: None,
                    ty: Ty::actor(Ty::Bound(r)),
                }],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("StdMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        {
            self.add_builtin_ctor(
                "Write",
                "StdMsg",
                vec![FieldInfo {
                    label: None,
                    ty: Ty::Con("String".into()),
                }],
                Scheme {
                    vars: vec![],
                    body: Ty::Fun(
                        vec![Ty::Con("String".into())],
                        Box::new(Ty::Con("StdMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        // ReadFile : String path, Actor reply -> FileMsg  (§5.5.7 file reading, D082)
        {
            let r = 0;
            self.add_builtin_ctor(
                "ReadFile",
                "FileMsg",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::Con("String".into()),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::actor(Ty::Bound(r)),
                    },
                ],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::Con("String".into()), Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("FileMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        // Net — user-facing request ctors on `NetMsg` (§5.5.7, token-based
        // protocol). Reply ctors (`Connected`/`Data`/`Closed` on `NetReply`)
        // are prelude-declared (D043). Reply targets stay flexible `Actor ?r`
        // (v1 has no polymorphic type syntax).
        {
            let r = 0;
            self.add_builtin_ctor(
                "RequestConnection",
                "NetMsg",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::int(),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::actor(Ty::Bound(r)),
                    },
                ],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::int(), Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("NetMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        {
            let r = 0;
            self.add_builtin_ctor(
                "RequestData",
                "NetMsg",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::int(),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::actor(Ty::Bound(r)),
                    },
                ],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::int(), Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("NetMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        self.add_builtin_ctor(
            "Send",
            "NetMsg",
            vec![
                FieldInfo {
                    label: None,
                    ty: Ty::int(),
                },
                FieldInfo {
                    label: None,
                    ty: Ty::list(Ty::int()),
                },
            ],
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::int(), Ty::list(Ty::int())],
                    Box::new(Ty::Con("NetMsg".into())),
                ),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "Close",
            "NetMsg",
            vec![FieldInfo {
                label: None,
                ty: Ty::int(),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::Con("NetMsg".into()))),
            constraints: vec![],            },
        );
        // Raw `@SysNet` protocol (D043 two-layer I/O): token-based routing to
        // per-connection `@SysConn` actors. `@SysNet` is predeclared above so
        // the prelude `@net` can address it; the reply targets stay flexible.
        {
            let r = 0;
            self.add_builtin_ctor(
                "ConnRequest",
                "SysNetMsg",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::int(),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::actor(Ty::Bound(r)),
                    },
                ],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::int(), Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("SysNetMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        {
            let r = 0;
            self.add_builtin_ctor(
                "DataRequest",
                "SysNetMsg",
                vec![
                    FieldInfo {
                        label: None,
                        ty: Ty::int(),
                    },
                    FieldInfo {
                        label: None,
                        ty: Ty::actor(Ty::Bound(r)),
                    },
                ],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::int(), Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("SysNetMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        self.add_builtin_ctor(
            "NetSend",
            "SysNetMsg",
            vec![
                FieldInfo {
                    label: None,
                    ty: Ty::int(),
                },
                FieldInfo {
                    label: None,
                    ty: Ty::list(Ty::int()),
                },
            ],
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::int(), Ty::list(Ty::int())],
                    Box::new(Ty::Con("SysNetMsg".into())),
                ),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "NetClose",
            "SysNetMsg",
            vec![FieldInfo {
                label: None,
                ty: Ty::int(),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::Con("SysNetMsg".into()))),
            constraints: vec![],            },
        );
        // Raw `@SysNet` replies (delivered to the prelude `@net` actor).
        self.add_builtin_ctor(
            "ConnToken",
            "SysNetReply",
            vec![FieldInfo {
                label: None,
                ty: Ty::int(),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::Con("SysNetReply".into()))),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "NetData",
            "SysNetReply",
            vec![FieldInfo {
                label: None,
                ty: Ty::list(Ty::int()),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::list(Ty::int())],
                    Box::new(Ty::Con("SysNetReply".into())),
                ),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "ConnGone",
            "SysNetReply",
            vec![FieldInfo {
                label: None,
                ty: Ty::int(),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::Con("SysNetReply".into()))),
            constraints: vec![],            },
        );
        // `@SysConn` inbox (per-connection native actor; only `@SysNet`
        // addresses it — never predeclared, so unnameable from WAG code).
        {
            let r = 0;
            self.add_builtin_ctor(
                "ConnRequestData",
                "ConnMsg",
                vec![FieldInfo {
                    label: None,
                    ty: Ty::actor(Ty::Bound(r)),
                }],
                Scheme {
                    vars: vec![r],
                    body: Ty::Fun(
                        vec![Ty::actor(Ty::Bound(r))],
                        Box::new(Ty::Con("ConnMsg".into())),
                    ),
                constraints: vec![],                },
            );
        }
        self.add_builtin_ctor(
            "ConnSend",
            "ConnMsg",
            vec![FieldInfo {
                label: None,
                ty: Ty::list(Ty::int()),
            }],
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::list(Ty::int())],
                    Box::new(Ty::Con("ConnMsg".into())),
                ),
            constraints: vec![],            },
        );
        self.add_builtin_ctor(
            "ConnClose",
            "ConnMsg",
            vec![],
            Scheme {
                vars: vec![],
                body: Ty::Con("ConnMsg".into()),
            constraints: vec![],            },
        );

        // Primitives as env schemes.
        // copy : &a -> a  (produces an owned value from a borrow)
        {
            let a = 0;
            self.env.insert(
                ("copy".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(vec![Ty::Borrow(Box::new(Ty::Var(a)))], Box::new(Ty::Var(a))),
                constraints: vec![],                },
            );
        }
        // bitNot : Int -> Int  (bitwise NOT; `!` is Bool-only, D074)
        self.env.insert(
            ("bitNot".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::int())),
            constraints: vec![],            },
        );
        // String primitives (§3 / §9). `String` is a non-pointer-free, linear
        // type — a packed UTF-8 byte buffer in the arena. Read-only operations
        // borrow their string argument (a non-consuming use, §4.3.3) so the
        // caller keeps ownership. Byte-level ops (stringLen/stringConcat/
        // stringByte/stringSlice/prefix-match) are prelude functions over
        // `Vec Int` (D073); only codepoint/Char bridges remain as builtins.
        // stringToCodepoints : &String -> Vec Int   (D088: Vec, was List)
        self.env.insert(
            ("stringToCodepoints".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Borrow(Box::new(Ty::Con("String".into())))],
                    Box::new(Ty::App("Vec".into(), vec![Ty::int()])),
                ),
            constraints: vec![],            },
        );
        // codepointsToString : Vec Int -> String   (D088: Vec, was List)
        self.env.insert(
            ("codepointsToString".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::App("Vec".into(), vec![Ty::int()])],
                    Box::new(Ty::Con("String".into())),
                ),
            constraints: vec![],            },
        );

        // stringSplit / stringJoin live in the prelude (item-8 cutover): they
        // produce/consume `List String`, a prelude-declared type, so they are
        // expressible in source and no longer VM builtins.

        // Char bridge + string indexing (§3.10 / §3.11).
        // stringGet : &String, Int -> Option Char   (codepoint-indexed, bounds-checked)
        self.env.insert(
            ("stringGet".to_string(), 2),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Borrow(Box::new(Ty::Con("String".into()))), Ty::int()],
                    Box::new(Ty::option(Ty::Con("Char".into()))),
                ),
            constraints: vec![],            },
        );
        // charToCodepoint : Char -> Int
        self.env.insert(
            ("charToCodepoint".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::Con("Char".into())], Box::new(Ty::int())),
            constraints: vec![],            },
        );
        // codepointToChar : Int -> Option Char      (total: None for surrogates / > U+10FFFF)
        self.env.insert(
            ("codepointToChar".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::int()],
                    Box::new(Ty::option(Ty::Con("Char".into()))),
                ),
            constraints: vec![],            },
        );


        // Array T — fixed-size, index-addressable, unconditionally non-pointer-free
        // (§3.4, §6.8). `T` is resolved by unification with how the array is used (§3.5).
        // arrNew : Int -> Arr (Option T)   (n slots, each None)
        {
            let a = 0;
            self.env.insert(
                ("arrNew".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![Ty::int()],
                        Box::new(Ty::App("Arr".into(), vec![Ty::option(Ty::Var(a))])),
                    ),
                constraints: vec![],                },
            );
        }
        // arrLen : &Arr T -> Int   (borrowed, non-consuming)
        {
            let a = 0;
            self.env.insert(
                ("arrLen".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![Ty::Borrow(Box::new(Ty::App(
                            "Arr".into(),
                            vec![Ty::Var(a)],
                        )))],
                        Box::new(Ty::int()),
                    ),
                constraints: vec![],                },
            );
        }
        // arrGet : &Arr T, Int -> Option T   (borrowed, bounds-checked)
        {
            let a = 0;
            self.env.insert(
                ("arrGet".to_string(), 2),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![
                            Ty::Borrow(Box::new(Ty::App("Arr".into(), vec![Ty::Var(a)]))),
                            Ty::int(),
                        ],
                        Box::new(Ty::option(Ty::Var(a))),
                    ),
                constraints: vec![],                },
            );
        }
        // arrSet : Arr T, Int, T -> Arr T   (consumes the arr — linear; OOB traps)
        {
            let a = 0;
            self.env.insert(
                ("arrSet".to_string(), 3),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![
                            Ty::App("Arr".into(), vec![Ty::Var(a)]),
                            Ty::int(),
                            Ty::Var(a),
                        ],
                        Box::new(Ty::App("Arr".into(), vec![Ty::Var(a)])),
                    ),
                constraints: vec![],                },
            );
        }
        // arrFromList : List T -> Arr T   (consumes the list)
        {
            let a = 0;
            self.env.insert(
                ("arrFromList".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![Ty::list(Ty::Var(a))],
                        Box::new(Ty::App("Arr".into(), vec![Ty::Var(a)])),
                    ),
                constraints: vec![],                },
            );
        }
        // arrToList : Arr T -> List T   (consumes the arr)
        {
            let a = 0;
            self.env.insert(
                ("arrToList".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(
                        vec![Ty::App("Arr".into(), vec![Ty::Var(a)])],
                        Box::new(Ty::list(Ty::Var(a))),
                    ),
                constraints: vec![],                },
            );
        }

        // stringByteAt : &String, Int -> Option Int   (byte-indexed, bounds-checked)
        self.env.insert(
            ("stringByteAt".to_string(), 2),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![
                        Ty::Borrow(Box::new(Ty::Con("String".into()))),
                        Ty::int(),
                    ],
                    Box::new(Ty::option(Ty::int())),
                ),
            constraints: vec![],            },
        );
        // stringToBytes : &String -> Vec Int   (extract byte representation, D088)
        self.env.insert(
            ("stringToBytes".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Borrow(Box::new(Ty::Con("String".into())))],
                    Box::new(Ty::App("Vec".into(), vec![Ty::int()])),
                ),
            constraints: vec![],            },
        );
        // bytesToString : Vec Int -> String   (build string from bytes, D088)
        self.env.insert(
            ("bytesToString".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::App("Vec".into(), vec![Ty::int()])],
                    Box::new(Ty::Con("String".into())),
                ),
            constraints: vec![],            },
        );

        // ─── Tier-2 primitives (S4, §9) ──────────────────────────────────────
        // nowMs : Int   (current time in ms since epoch; host clock)
        self.env.insert(
            ("nowMs".to_string(), 0),
            Scheme {
                vars: vec![],
                body: Ty::Fun(Vec::new(), Box::new(Ty::int())),
            constraints: vec![],            },
        );
        // randRange : Int, Int -> Int   (uniform in [lo, hi]; traps if lo > hi)
        self.env.insert(
            ("randRange".to_string(), 2),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int(), Ty::int()], Box::new(Ty::int())),
            constraints: vec![],            },
        );
        // envGet : String -> Option String   (environment variable lookup)
        self.env.insert(
            ("envGet".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Con("String".into())],
                    Box::new(Ty::option(Ty::Con("String".into()))),
                ),
            constraints: vec![],            },
        );
        // assert : Bool, String -> Unit   (test infra): panics with the message if
        // the condition is false. Pure from the type's point of view; only its
        // runtime effect (a VmError::Panic) signals failure to `wagc test`.
        self.env.insert(
            ("assert".to_string(), 2),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::bool(), Ty::Con("String".into())],
                    Box::new(Ty::unit()),
                ),
            constraints: vec![],            },
        );

        // ─── File I/O (S5, simplified as builtins for v1) ──────────────────
        // fileOpenRead : String -> Option Int   (returns fd, None on failure)
        self.env.insert(
            ("fileOpenRead".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Con("String".into())],
                    Box::new(Ty::option(Ty::int())),
                ),
            constraints: vec![],            },
        );
        // fileOpenWrite : String -> Option Int   (returns fd, None on failure)
        self.env.insert(
            ("fileOpenWrite".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(
                    vec![Ty::Con("String".into())],
                    Box::new(Ty::option(Ty::int())),
                ),
            constraints: vec![],            },
        );
        // fileRead : Int -> Option (List Int)   (reads up to 4096 bytes; None = EOF/error)
        self.env.insert(
            ("fileRead".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::option(Ty::list(Ty::int())))),
            constraints: vec![],            },
        );
        // fileWrite : Int, List Int -> Int   (writes bytes; returns count written, -1 on error)
        self.env.insert(
            ("fileWrite".to_string(), 2),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int(), Ty::list(Ty::int())], Box::new(Ty::int())),
            constraints: vec![],            },
        );
        // fileClose : Int -> Int   (closes fd; returns 0 on success, -1 on error)
        self.env.insert(
            ("fileClose".to_string(), 1),
            Scheme {
                vars: vec![],
                body: Ty::Fun(vec![Ty::int()], Box::new(Ty::int())),
            constraints: vec![],            },
        );

        // Register the parametric type constructors in `type_ctors` so the
        // alias-vs-datatype disambiguation pass (§3.2) and method-receiver
        // resolution recognise them. `Arr` is a builtin (no user ctor, built
        // via `arrNew`/`arrSet`); `List`/`Vec` are prelude-declared datatypes
        // (D054/D088) whose constructors come from the prelude.
        for name in ["Arr", "List", "Vec"] {
            self.type_ctors.entry(name.to_string()).or_default();
        }
        // Register type params so prelude method receivers resolve as
        // `App("Arr", [T])` (not bare `Con("Arr")`) and unify correctly.
        self.type_params.insert("Arr".to_string(), vec!["T".to_string()]);
        self.type_params.insert("Vec".to_string(), vec!["T".to_string()]);

        // Monad Option instance (standalone inference, no prelude loaded).
        // bind : Option A, (A -> Option B) -> Option B
        {
            let a = 0u32;
            let b = 1u32;
            self.op_impl_env.insert(
                ("__bind_Monad_Option".to_string(), 2),
                Scheme {
                    vars: vec![a, b],
                    body: Ty::Fun(
                        vec![
                            Ty::option(Ty::Bound(a)),
                            Ty::Fun(vec![Ty::Bound(a)], Box::new(Ty::option(Ty::Bound(b)))),
                        ],
                        Box::new(Ty::option(Ty::Bound(b))),
                    ),
                constraints: vec![],            },
            );
        }
        // pure : A -> Option A
        {
            let a = 0u32;
            self.op_impl_env.insert(
                ("__pure_Monad_Option".to_string(), 1),
                Scheme {
                    vars: vec![a],
                    body: Ty::Fun(vec![Ty::Bound(a)], Box::new(Ty::option(Ty::Bound(a)))),
                constraints: vec![],            },
            );
        }
    }

    pub(super) fn add_builtin_ctor(
        &mut self,
        name: &str,
        type_name: &str,
        fields: Vec<FieldInfo>,
        scheme: Scheme,
    ) {
        let arity = fields.len();
        self.ctors.insert(
            name.into(),
            CtorInfo {
                name: name.into(),
                type_name: type_name.into(),
                fields,
                n_params: scheme.vars.len(),
                scheme: scheme.clone(),
            },
        );
        self.type_ctors
            .entry(type_name.into())
            .or_default()
            .push(name.into());
        // Constructor arity = number of fields (params it takes).
        self.env.insert((name.to_string(), arity), scheme);
    }
}

/// Monomorphic builtin function types for checker-side local inference.
///
/// Returns `(name, arity) → Ty` for all builtins that appear as callable
/// functions in user code. Polymorphic schemes are instantiated with fresh
/// type variables (sufficient for field-path resolution which only needs the
/// result-type head). Consumed by `wag-check`'s `FieldTable::build`.
pub fn builtin_function_types() -> std::collections::BTreeMap<(String, usize), Ty> {
    use std::collections::BTreeMap;
    let mut m = BTreeMap::new();

    // copy : &a -> a  (polymorphic — instantiate with Var(0))
    m.insert(("copy".into(), 1), Ty::Fun(vec![Ty::Borrow(Box::new(Ty::Var(0)))], Box::new(Ty::Var(0))));


    // String primitives (§3 / §9) — codepoint/Char bridges only (D073, D088: Vec).
    m.insert(("stringToCodepoints".into(), 1), Ty::Fun(vec![Ty::Borrow(Box::new(Ty::Con("String".into())))], Box::new(Ty::App("Vec".into(), vec![Ty::int()]))));
    m.insert(("codepointsToString".into(), 1), Ty::Fun(vec![Ty::App("Vec".into(), vec![Ty::int()])], Box::new(Ty::Con("String".into()))));
    m.insert(("stringGet".into(), 2), Ty::Fun(vec![Ty::Borrow(Box::new(Ty::Con("String".into()))), Ty::int()], Box::new(Ty::option(Ty::Con("Char".into())))));
    m.insert(("charToCodepoint".into(), 1), Ty::Fun(vec![Ty::Con("Char".into())], Box::new(Ty::int())));
    m.insert(("codepointToChar".into(), 1), Ty::Fun(vec![Ty::int()], Box::new(Ty::option(Ty::Con("Char".into())))));

    // File I/O
    m.insert(("fileOpenRead".into(), 1), Ty::Fun(vec![Ty::Con("String".into())], Box::new(Ty::option(Ty::int()))));
    m.insert(("fileOpenWrite".into(), 1), Ty::Fun(vec![Ty::Con("String".into())], Box::new(Ty::option(Ty::int()))));
    m.insert(("fileRead".into(), 1), Ty::Fun(vec![Ty::int()], Box::new(Ty::option(Ty::list(Ty::int())))));
    m.insert(("fileWrite".into(), 2), Ty::Fun(vec![Ty::int(), Ty::list(Ty::int())], Box::new(Ty::int())));
    m.insert(("fileClose".into(), 1), Ty::Fun(vec![Ty::int()], Box::new(Ty::int())));

    // Misc
    m.insert(("nowMs".into(), 0), Ty::Fun(vec![], Box::new(Ty::int())));
    m.insert(("randRange".into(), 2), Ty::Fun(vec![Ty::int(), Ty::int()], Box::new(Ty::int())));
    m.insert(("envGet".into(), 1), Ty::Fun(vec![Ty::Con("String".into())], Box::new(Ty::option(Ty::Con("String".into())))));

    m
}
