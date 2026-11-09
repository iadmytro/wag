//! User constructor registration and namespacing checks.

use std::collections::HashMap;

use wag_syntax::{ConstructorDecl, ParametricTypeDecl, Span};

use crate::error::{Stage, TypeError};
use crate::types::{CtorInfo, FieldInfo, Scheme, Ty};

use super::context::Context;
use super::util::{surface_to_ty_resolved, surface_to_ty_with_bound};

impl Context {
    /// Per-module namespacing check: ctor names must be unique *within* a
    /// module; the same name may appear in different modules (import aliasing
    /// disambiguates at use sites). Reserved built-in names are rejected
    /// globally.
    pub(super) fn check_namespacing(
        &self,
        user: &[(String, ConstructorDecl)],
    ) -> Result<(), TypeError> {
        // I/O request ctors are checker built-ins (reserved). User-facing
        // reply ctors (`Line`/`EndOfInput`, `Connected`/`Data`/`Closed`) are
        // prelude-declared, so a user redeclaration is caught by the duplicate
        // check below (the prelude is merged into `user_ctors`).
        let reserved = [
            "True",
            "False",
            "Pair",
            "Triple",
            "Unit",
            "RequestLine",
            "Write",
            "ReadFile",
            "RequestConnection",
            "RequestData",
            "Send",
            "Close",
            "ConnRequest",
            "DataRequest",
            "NetSend",
            "NetClose",
            "ConnToken",
            "NetData",
            "ConnGone",
            "ConnRequestData",
            "ConnSend",
            "ConnClose",
        ];
        let mut seen: HashMap<(String, String), Span> = HashMap::new();
        for (module, c) in user {
            if reserved.contains(&c.name.as_str()) {
                return Err(TypeError::new(
                    Stage::Namespacing,
                    c.span,
                    format!("constructor `{}` collides with a built-in name", c.name),
                ));
            }
            let key = (module.clone(), c.name.clone());
            if let Some(prev) = seen.insert(key, c.span) {
                return Err(TypeError::new(
                    Stage::Namespacing,
                    c.span,
                    format!(
                        "duplicate constructor `{}` in module `{}` (previous at {}..{})",
                        c.name, module, prev.start, prev.end
                    ),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn add_user_ctor(&mut self, c: &ConstructorDecl) -> Result<(), TypeError> {
        let type_name = c.result_type.head_name().ok_or_else(|| {
            TypeError::new(
                Stage::Namespacing,
                c.span,
                "constructor result type must be a named type",
            )
        })?;
        // Convert field types; result is Con(type_name) for nullary user types.
        let mut field_tys = Vec::new();
        for f in &c.fields {
            field_tys.push(FieldInfo {
                label: f.label.clone(),
                ty: surface_to_ty_resolved(&f.ty, &self.aliases)?,
            });
        }
        let result_ty = surface_to_ty_resolved(&c.result_type, &self.aliases)?;
        let fun_ty = if field_tys.is_empty() {
            result_ty.clone()
        } else {
            Ty::Fun(
                field_tys.iter().map(|f| f.ty.clone()).collect(),
                Box::new(result_ty),
            )
        };
        let scheme = Scheme::mono(fun_ty);
        self.ctors.insert(
            c.name.clone(),
            CtorInfo {
                name: c.name.clone(),
                type_name: type_name.clone(),
                fields: field_tys,
                n_params: 0, // monomorphic user ctor
                scheme: scheme.clone(),
            },
        );
        self.type_ctors
            .entry(type_name)
            .or_default()
            .push(c.name.clone());
        self.env.insert((c.name.clone(), c.fields.len()), scheme);
        Ok(())
    }

    /// Register a parametric type's constructors with a polymorphic scheme.
    ///
    /// For a parametric type `MyPair a b` with constructor `MyPair: a, b`
    /// (declaration space uses `Bound`, A1b 3b):
    /// - type vars: `a` → Bound(0), `b` → Bound(1)
    /// - field types: [Bound(0), Bound(1)]
    /// - result type: App("MyPair", [Bound(0), Bound(1)])
    /// - scheme: forall [0, 1]. Fun([Bound(0), Bound(1)], App("MyPair", [Bound(0), Bound(1)]))
    pub(super) fn add_parametric_type(&mut self, pt: &ParametricTypeDecl) -> Result<(), TypeError> {
        // Record declared param names for method-receiver binding (D061).
        self.type_params
            .insert(pt.name.clone(), pt.type_vars.clone());
        // Map type variable names to indices.
        let type_vars: HashMap<String, u32> = pt
            .type_vars
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i as u32))
            .collect();
        let var_indices: Vec<u32> = (0..pt.type_vars.len() as u32).collect();

        for c in &pt.ctors {
            let type_name = c.result_type.head_name().ok_or_else(|| {
                TypeError::new(
                    Stage::Namespacing,
                    c.span,
                    "parametric constructor result type must be a named type",
                )
            })?;

            // Convert field types with declaration-space param resolution (A1b 3b).
            let mut field_tys = Vec::new();
            for f in &c.fields {
                field_tys.push(FieldInfo {
                    label: f.label.clone(),
                    ty: surface_to_ty_with_bound(&f.ty, &type_vars)?,
                });
            }

            // Convert result type with declaration-space param resolution.
            let result_ty = surface_to_ty_with_bound(&c.result_type, &type_vars)?;

            let fun_ty = if field_tys.is_empty() {
                result_ty.clone()
            } else {
                Ty::Fun(
                    field_tys.iter().map(|f| f.ty.clone()).collect(),
                    Box::new(result_ty),
                )
            };

            // Polymorphic scheme with the type variables.
            let scheme = Scheme {
                vars: var_indices.clone(),
                body: fun_ty,
            constraints: vec![],            };

            self.ctors.insert(
                c.name.clone(),
                CtorInfo {
                    name: c.name.clone(),
                    type_name: type_name.clone(),
                    fields: field_tys,
                    n_params: var_indices.len(),
                    scheme: scheme.clone(),
                },
            );
            self.type_ctors
                .entry(type_name)
                .or_default()
                .push(c.name.clone());
            self.env.insert((c.name.clone(), c.fields.len()), scheme);
        }

        Ok(())
    }
}
