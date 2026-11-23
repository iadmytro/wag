//! Pretty-printing for the AST, used by the `WAG_DEBUG` pipeline dumps.

use crate::ast::*;

/// BinOp → operator symbol.
fn op_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Gt => ">",
        BinOp::Lt => "<",
        BinOp::Ge => ">=",
        BinOp::Le => "<=",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::BitNot => "!",
        BinOp::ShiftLeft => "<<",
        BinOp::ShiftRight => ">>",
    }
}

/// Render an expression to a single-line string.
pub fn pretty_expr(e: &Expr) -> String {
    let mut s = String::new();
    fmt_expr(e, &mut s);
    s
}

fn fmt_expr(e: &Expr, s: &mut String) {
    match e {
        Expr::Int { value, .. } => s.push_str(value.to_string().as_str()),
        Expr::Str { value, .. } => s.push_str(&format!("\"{value}\"")),
        Expr::Var { name, .. } => s.push_str(name),
        Expr::Upper { name, .. } => s.push_str(name),
        Expr::AtName { name, .. } => s.push_str(&format!("@{name}")),
        Expr::Self_ { .. } => s.push_str("self"),
        Expr::Apply { fun, args, .. } => {
            fmt_expr(fun, s);
            for a in args {
                s.push(' ');
                fmt_expr(a, s);
            }
        }
        Expr::Send { target, msg, .. } => {
            // `ident@ msg` — print the target name with trailing @.
            if let Expr::Var { name, .. } = target.as_ref() {
                s.push_str(name);
                s.push('@');
            } else {
                fmt_expr(target, s);
                s.push('@');
            }
            s.push(' ');
            fmt_expr(msg, s);
        }
        Expr::BinOp { op, lhs, rhs, .. } => {
            s.push('(');
            fmt_expr(lhs, s);
            let sym = op_symbol(op);
            s.push_str(&format!(" {sym} "));
            fmt_expr(rhs, s);
            s.push(')');
        }
        Expr::Field { base, field, .. } => {
            fmt_expr(base, s);
            s.push('.');
            s.push_str(field);
        }
        Expr::RecordUpdate { receiver, clauses, .. } => {
            // D048 bare form: `<receiver>.<path> <- <value>` (method-body write syntax).
            fmt_expr(receiver, s);
            for (i, c) in clauses.iter().enumerate() {
                if i > 0 {
                    s.push_str("; ");
                }
                fmt_update_clause(c, s);
            }
        }
        Expr::SetField { receiver, field_idx, value, .. } => {
            s.push_str("(set ");
            fmt_expr(receiver, s);
            s.push(' ');
            s.push_str(&field_idx.to_string());
            s.push_str(" = ");
            fmt_expr(value, s);
            s.push(')');
        }
        Expr::RegionWrite { receiver, path, value, .. } => {
            s.push_str("(region-write ");
            fmt_expr(receiver, s);
            for p in path {
                s.push('.');
                s.push_str(&p.to_string());
            }
            s.push_str(" = ");
            fmt_expr(value, s);
            s.push(')');
        }
        Expr::Borrow { inner, .. } => {
            s.push('&');
            fmt_expr(inner, s);
        }
        Expr::Bind { name, value, body, .. } => {
            s.push_str(&format!("let {name} = "));
            fmt_expr(value, s);
            s.push_str(" in ");
            fmt_expr(body, s);
        }
        Expr::Seq { first, second, .. } => {
            s.push_str("(");
            fmt_expr(first, s);
            s.push_str("; ");
            fmt_expr(second, s);
            s.push_str(")");
        }
        Expr::If { arms, .. } => {
            s.push_str("if ");
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    s.push_str(" / ");
                }
                match &arm.condition {
                    Some(c) => {
                        fmt_expr(c, s);
                        s.push_str(" -> ");
                    }
                    None => s.push_str("_ -> "),
                }
                fmt_expr(&arm.body, s);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            s.push_str("match ");
            fmt_expr(scrutinee, s);
            s.push_str(" { ");
            for (i, arm) in arms.iter().enumerate() {
                if i > 0 {
                    s.push_str("; ");
                }
                fmt_pat(&arm.pattern, s);
                s.push_str(" -> ");
                fmt_expr(&arm.body, s);
            }
            s.push('}');
        }
        Expr::Do { stmts, .. } => {
            s.push_str("do { ");
            for (i, stmt) in stmts.iter().enumerate() {
                if i > 0 {
                    s.push_str("; ");
                }
                match stmt {
                    DoStmt::Bind { name, expr } => {
                        s.push_str(name);
                        s.push_str(" <- ");
                        fmt_expr(expr, s);
                    }
                    DoStmt::Return(e) => {
                        s.push_str("return ");
                        fmt_expr(e, s);
                    }
                }
            }
            s.push('}');
        }
        Expr::ListLit { elems, .. } => {
            s.push('[');
            for (i, el) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                fmt_expr(el, s);
            }
            s.push(']');
        }
        Expr::Lambda { params, body, .. } => {
            s.push('(');
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(p);
            }
            s.push_str(" -> ");
            fmt_expr(body, s);
            s.push(')');
        }
        Expr::Comprehension {
            generators,
            accs,
            guards,
            result,
            ..
        } => {
            s.push('[');
            for (i, g) in generators.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                for (j, v) in g.vars.iter().enumerate() {
                    if j > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(v);
                }
                s.push_str(" <- ");
                for (j, src) in g.sources.iter().enumerate() {
                    if j > 0 {
                        s.push_str(", ");
                    }
                    fmt_expr(src, s);
                }
            }
            if !accs.is_empty() {
                s.push_str(" | ");
                for (i, a) in accs.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&format!("{} = ", a.name));
                    fmt_expr(&a.init, s);
                }
            }
            s.push_str(" | ");
            for (i, g) in guards.iter().enumerate() {
                if i > 0 {
                    s.push_str(" && ");
                }
                fmt_expr(g, s);
            }
            s.push_str(" -> ");
            fmt_expr(result, s);
            s.push(']');
        }
        Expr::Product { elems, .. } => {
            s.push('(');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                fmt_expr(e, s);
            }
            s.push(')');
        }
        Expr::Unit { .. } => s.push_str("()"),
    }
}

fn fmt_update_clause(c: &UpdateClause, s: &mut String) {
    match c {
        UpdateClause::Leaf { path, value, .. } => {
            s.push_str(&format!(".{}", path.join(".")));
            s.push_str(" <- ");
            fmt_expr(value, s);
        }
        UpdateClause::Group { path, clauses, .. } => {
            s.push_str(&format!(".{}", path.join(".")));
            s.push_str(" { ");
            for (i, cc) in clauses.iter().enumerate() {
                if i > 0 {
                    s.push_str("; ");
                }
                fmt_update_clause(cc, s);
            }
            s.push('}');
        }
    }
}

fn fmt_pat(p: &Pat, s: &mut String) {
    match p {
        Pat::Wildcard { .. } => s.push('_'),
        Pat::Var { name, .. } => s.push_str(name),
        Pat::Ctor { name, args, .. } => {
            s.push_str(name);
            for a in args {
                s.push(' ');
                fmt_pat(a, s);
            }
        }
        Pat::Literal { lit, .. } => match lit {
            LitPat::Int(v) => s.push_str(&v.to_string()),
            LitPat::Str(v) => {
                s.push('"');
                s.push_str(v);
                s.push('"');
            }
        },
        Pat::Product { elems, .. } => {
            s.push('(');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                fmt_pat(e, s);
            }
            s.push(')');
        }
    }
}

/// Render a type to a string.
pub fn pretty_type(t: &Type) -> String {
    let mut s = String::new();
    fmt_type(t, &mut s);
    s
}

fn fmt_type(t: &Type, s: &mut String) {
    match t {
        Type::Name(n) => s.push_str(n),
        Type::App(head, args) => {
            fmt_type(head, s);
            for a in args {
                s.push(' ');
                fmt_type(a, s);
            }
        }
        Type::Borrow(inner) => {
            s.push('&');
            fmt_type(inner, s);
        }
        Type::Func(params, ret) => {
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                fmt_type(p, s);
            }
            if !params.is_empty() {
                s.push_str(" -> ");
            }
            fmt_type(ret, s);
        }
        Type::Product(types) => {
            s.push('(');
            for (i, t) in types.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                fmt_type(t, s);
            }
            s.push(')');
        }
    }
}

/// Render a whole program (all items) to a readable string.
pub fn pretty_program(program: &Program) -> String {
    let mut s = String::new();
    for item in &program.items {
        match item {
            Item::Func(f) => {
                if f.params.is_empty() && f.properties.is_empty() && f.signature.is_none() {
                    // Nullary value: `name = body`
                    s.push_str(&format!("{} = {}\n", f.name, pretty_expr(&f.body)));
                } else {
                    // Unified form (D034): `name: [properties:] [typeSig:] params = body`
                    let mut parts = Vec::new();
                    if !f.properties.is_empty() {
                        parts.push(f.properties.join(", "));
                    }
                    if let Some(sig) = &f.signature {
                        parts.push(pretty_type(sig));
                    }
                    parts.push(format!("{} = {}", f.params.join(" "), pretty_expr(&f.body)));
                    s.push_str(&format!("{}: {}\n", f.name, parts.join(": ")));
                }
            }
            Item::Method(m) => {
                let mut parts = Vec::new();
                if let Some(sig) = &m.signature {
                    parts.push(pretty_type(sig));
                }
                parts.push(format!("{} = {}", m.params.join(" "), pretty_expr(&m.body)));
                s.push_str(&format!("{}.{}: {}\n", m.type_name, m.name, parts.join(": ")));
            }
            Item::Actor(a) => {
                s.push_str(&format!("@{} {} = {}\n", a.name, a.params.join(" "), pretty_expr(&a.body)));
            }
            Item::Constructor(c) => {
                let fields = c
                    .fields
                    .iter()
                    .map(|f| match &f.label {
                        Some(l) => format!("{l} {}", pretty_type(&f.ty)),
                        None => pretty_type(&f.ty),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                s.push_str(&format!(
                    "{} = {} [{}] : {}\n",
                    c.name,
                    fields,
                    c.fields.len(),
                    pretty_type(&c.result_type)
                ));
            }
            Item::TypeAlias(a) => {
                s.push_str(&format!("{} = {}\n", a.name, pretty_type(&a.ty)));
            }
            Item::ParametricType(p) => {
                s.push_str(&format!(
                    "{} {}\n",
                    p.name,
                    p.type_vars.join(" ")
                ));
                for c in &p.ctors {
                    let fields = c
                        .fields
                        .iter()
                        .map(|f| match &f.label {
                            Some(l) => format!("{l} {}", pretty_type(&f.ty)),
                            None => pretty_type(&f.ty),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    s.push_str(&format!(
                        "  {} = {} [{}] : {}\n",
                        c.name,
                        fields,
                        c.fields.len(),
                        pretty_type(&c.result_type)
                    ));
                }
            }
            Item::Class(c) => {
                s.push_str(&format!("{} {}\n", c.name, c.params.join(" ")));
                for m in &c.methods {
                    s.push_str(&format!("  {} : {}\n", m.name, pretty_type(&m.sig)));
                }
            }
            Item::Instance(i) => {
                s.push_str(&format!("{} {}\n", i.class_name, pretty_type(&i.instance_ty)));
                for f in &i.methods {
                    s.push_str(&format!(
                        "  {} {} = {}\n",
                        f.name,
                        f.params.join(" "),
                        pretty_expr(&f.body)
                    ));
                }
            }
            Item::ConstrainedDatatype(c) => {
                s.push_str(&format!(
                    "{} = {}: {} -> {}\n",
                    c.name,
                    pretty_type(&c.sig),
                    c.param,
                    pretty_expr(&c.body)
                ));
            }
        }
    }
    s
}

/// True when `WAG_DEBUG` is set in the environment (any value).
pub fn debug_enabled() -> bool {
    std::env::var_os("WAG_DEBUG").is_some()
}

/// Print a labeled pipeline dump to stderr, only when `WAG_DEBUG` is set.
pub fn debug_dump(label: &str, program: &Program) {
    if !debug_enabled() {
        return;
    }
    eprintln!("\n===== WAG_DEBUG: {label} =====");
    eprintln!("{}", pretty_program(program));
}
