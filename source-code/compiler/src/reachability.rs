use hsharp_parser::ast::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// Names that are always considered reachable regardless of the call
/// graph — the program's actual entry point(s). `main` is the only real
/// one for a compiled binary; `pub fn`s are NOT included here on purpose
/// (a std file's `pub fn` being exported doesn't mean any function in
/// THIS particular compiled program calls it — that's exactly the case
/// this pass exists to stop over-including).
fn is_entry_point(name: &str) -> bool {
    name == "main"
}

/// Returns the set of function names reachable from an entry point,
/// by name, ignoring which module/file each originated from (this
/// compiler already resolves calls by bare/last-path-segment name only —
/// see `parser.rs`'s "H# struct and enum names are looked up globally by
/// their *bare* name" comment for the equivalent, already-established
/// convention for types; the same is true for inlined function names).
pub fn compute_reachable(module: &Module) -> HashSet<String> {
    let mut bodies: HashMap<String, &[Stmt]> = HashMap::new();
    collect_fn_bodies(&module.items, &mut bodies);

    let mut reachable: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for name in bodies.keys() {
        if is_entry_point(name) {
            reachable.insert(name.clone());
            queue.push_back(name.clone());
        }
    }
    // `impl` block methods are always seeded as reachable, never pruned:
    // a `.method()` call site (`Expr::MethodCall`) is dispatched by the
    // receiver's inferred type, which this walker doesn't do full type
    // inference to replicate — so it can't safely tell which mangled
    // `TypeName_method` name (see `collect_fns`'s own mangling in
    // codegen.rs) a given call site resolves to. Under-pruning method
    // bodies (by never pruning them at all) is always safe; the actual
    // risk this whole pass exists to avoid is the opposite mistake
    // (wrongly treating something AS dead). Free functions (everything
    // this pass actually targets — every real case that motivated it,
    // std files full of plain top-level `fn`s) are unaffected.
    for name in bodies.keys() {
        if name.contains("__impl_method__") {
            reachable.insert(name.clone());
        }
    }
    // No recognizable entry point at all (e.g. a library-only file with
    // no `main`, or a standalone `hsharp check` on a std file by itself)
    // — fall back to treating every function as reachable, since there's
    // no safe notion of "dead" without knowing who the real caller is.
    if queue.is_empty() && reachable.is_empty() {
        return bodies.keys().cloned().collect();
    }

    while let Some(name) = queue.pop_front() {
        let Some(body) = bodies.get(name.as_str()) else { continue };
        let mut callees = HashSet::new();
        for stmt in *body {
            collect_calls_stmt(stmt, &mut callees);
        }
        for callee in callees {
            if reachable.insert(callee.clone()) {
                queue.push_back(callee);
            }
        }
    }
    reachable
}

fn collect_fn_bodies<'a>(items: &'a [Item], out: &mut HashMap<String, &'a [Stmt]>) {
    for item in items {
        match item {
            Item::FnDef(f) => { out.insert(f.name.clone(), &f.body); }
            Item::ImplBlock(imp) => {
                for m in &imp.methods {
                    // Tagged (see `compute_reachable`'s seeding loop above)
                    // rather than given codegen's real mangled name —
                    // this map's keys only need to be internally
                    // consistent for the BFS below; `compute_reachable`
                    // returns names to `features.rs`, which matches them
                    // against plain `FnDef.name`/`ImplBlock` method names
                    // one level up (`check_item`'s own `Item::ImplBlock`
                    // arm), never against this synthetic key.
                    out.insert(format!("__impl_method__{}", m.name), &m.body);
                }
            }
            // Inlined `mod x` / `use "std -> x"` content — same flat,
            // bare-name namespace as everything else once inlined (see
            // this file's module doc comment).
            Item::ModDecl { inline: Some(inner), .. } => collect_fn_bodies(inner, out),
            _ => {}
        }
    }
}

fn collect_calls_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Let { value: Some(e), .. } => collect_calls_expr(e, out),
        Stmt::Expr(e, _) => collect_calls_expr(e, out),
        Stmt::Return(Some(e), _) => collect_calls_expr(e, out),
        Stmt::Break(Some(e), _) => collect_calls_expr(e, out),
        Stmt::Item(Item::FnDef(f)) => { for s in &f.body { collect_calls_stmt(s, out); } }
        Stmt::Item(Item::ModDecl { inline: Some(inner), .. }) => {
            for it in inner {
                if let Item::FnDef(f) = it {
                    for s in &f.body { collect_calls_stmt(s, out); }
                }
            }
        }
        _ => {}
    }
}

fn callee_name(callee: &Expr) -> Option<String> {
    match callee {
        Expr::Ident(n, _) => Some(n.clone()),
        // `module::function(...)` — this compiler resolves the call by
        // its last path segment only (mirrors how types resolve — see
        // this file's module doc comment), so that's the name that
        // matters for matching it back to a `FnDef.name`/builtin name.
        Expr::Path(segments, _) => segments.last().cloned(),
        _ => None,
    }
}

fn collect_calls_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Call(callee, args, _) => {
            if let Some(n) = callee_name(callee) { out.insert(n); }
            collect_calls_expr(callee, out);
            for a in args { collect_calls_expr(a, out); }
        }
        Expr::MethodCall(recv, _, args, _) => {
            collect_calls_expr(recv, out);
            for a in args { collect_calls_expr(a, out); }
        }
        Expr::BinOp(l, _, r, _) | Expr::Range(l, r, _, _) => {
            collect_calls_expr(l, out); collect_calls_expr(r, out);
        }
        Expr::UnOp(_, e, _) | Expr::Cast(e, _, _) | Expr::Try(e, _) | Expr::Await(e, _) => {
            collect_calls_expr(e, out);
        }
        Expr::Assign(l, r, _) | Expr::CompoundAssign(l, _, r, _) => {
            collect_calls_expr(l, out); collect_calls_expr(r, out);
        }
        Expr::FieldAccess(e, _, _) => collect_calls_expr(e, out),
        Expr::IndexAccess(e, i, _) => { collect_calls_expr(e, out); collect_calls_expr(i, out); }
        Expr::ArrayLit(elems, _) | Expr::TupleLit(elems, _) => {
            for e in elems { collect_calls_expr(e, out); }
        }
        Expr::StructLit(_, fields, _) => { for (_, e) in fields { collect_calls_expr(e, out); } }
        Expr::Return(Some(e), _) => collect_calls_expr(e, out),
        Expr::If { condition, then_body, elsif_branches, else_body, .. } => {
            collect_calls_expr(condition, out);
            for s in then_body { collect_calls_stmt(s, out); }
            for (c, body) in elsif_branches {
                collect_calls_expr(c, out);
                for s in body { collect_calls_stmt(s, out); }
            }
            if let Some(body) = else_body { for s in body { collect_calls_stmt(s, out); } }
        }
        Expr::Match { subject, arms, .. } => {
            collect_calls_expr(subject, out);
            for arm in arms {
                if let Some(g) = &arm.guard { collect_calls_expr(g, out); }
                for s in &arm.body { collect_calls_stmt(s, out); }
            }
        }
        Expr::While { condition, body, .. } => {
            collect_calls_expr(condition, out);
            for s in body { collect_calls_stmt(s, out); }
        }
        Expr::For { iterable, body, .. } => {
            collect_calls_expr(iterable, out);
            for s in body { collect_calls_stmt(s, out); }
        }
        Expr::Do { body, .. } | Expr::Unsafe(body, _, _) => {
            for s in body { collect_calls_stmt(s, out); }
        }
        Expr::Closure { body, .. } => {
            for s in body { collect_calls_stmt(s, out); }
        }
        _ => {}
    }
}
