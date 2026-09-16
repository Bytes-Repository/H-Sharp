use hsharp_hlib::{AbiType, ExportedSymbol, ExportedSymbolKind};
use hsharp_parser::ast::{EnumVariantFields, Item, Module, Param, TypeExpr};

/// Result of walking a module for `.hlib` export purposes.
pub struct ExportSummary {
    pub symbols: Vec<ExportedSymbol>,
    /// The `pub` subset of `module.items`, kept as real `ast::Item`
    /// values so the caller can `serde_json::to_vec_pretty` them
    /// (or bincode, if a more compact `ast` artifact is ever wanted —
    /// nothing here assumes JSON specifically) into the `ast` artifact.
    pub pub_items: Vec<Item>,
}

/// Walks `module.items` (recursing into inline `mod` blocks) and
/// collects every `pub` `fn`/`struct`/`enum`/`trait`/`const`/`type`
/// into an `ExportSummary`. `impl` blocks and non-`pub` items are
/// skipped — see the module doc comment above for why.
pub fn summarize_exports(module: &Module) -> ExportSummary {
    let mut symbols = Vec::new();
    let mut pub_items = Vec::new();
    walk_items(&module.items, &mut symbols, &mut pub_items);
    ExportSummary { symbols, pub_items }
}

fn walk_items(items: &[Item], symbols: &mut Vec<ExportedSymbol>, pub_items: &mut Vec<Item>) {
    for item in items {
        match item {
            Item::FnDef(f) if f.pub_ => {
                let generic = !f.type_params.is_empty();
                let params: Vec<AbiType> = f.params.iter().map(|p| lower_param_type(p)).collect();
                let returns = f.return_type.as_ref().and_then(lower_type_expr);
                symbols.push(ExportedSymbol {
                    name: f.name.clone(),
                    kind: ExportedSymbolKind::Function,
                    signature: render_fn_signature(&f.name, &f.params, &f.return_type),
                    params,
                    returns,
                    generic,
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::StructDef(s) if s.pub_ => {
                symbols.push(ExportedSymbol {
                    name: s.name.clone(),
                    kind: ExportedSymbolKind::Struct,
                    signature: format!("struct {}", s.name),
                    params: Vec::new(),
                    returns: None,
                    generic: !s.type_params.is_empty(),
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::EnumDef(e) if e.pub_ => {
                symbols.push(ExportedSymbol {
                    name: e.name.clone(),
                    kind: ExportedSymbolKind::Enum,
                    signature: format!(
                        "enum {} {{ {} }}",
                        e.name,
                        e.variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    params: Vec::new(),
                    returns: None,
                    generic: !e.type_params.is_empty()
                        || e.variants.iter().any(|v| matches!(v.fields, EnumVariantFields::Tuple(_) | EnumVariantFields::Struct(_))),
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::TraitDef(t) if t.pub_ => {
                symbols.push(ExportedSymbol {
                    name: t.name.clone(),
                    kind: ExportedSymbolKind::Trait,
                    signature: format!("trait {}", t.name),
                    params: Vec::new(),
                    returns: None,
                    // Traits are always AST-only (source-level dispatch) —
                    // there is no ABI-stable way to hand a consumer a
                    // vtable across the language boundary in v1.
                    generic: true,
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::ConstDef { name, ty, pub_: true, .. } => {
                symbols.push(ExportedSymbol {
                    name: name.clone(),
                    kind: ExportedSymbolKind::Const,
                    signature: format!("const {}", name),
                    params: Vec::new(),
                    returns: ty.as_ref().and_then(lower_type_expr),
                    generic: ty.as_ref().and_then(lower_type_expr).is_none(),
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::TypeAlias { name, pub_: true, .. } => {
                symbols.push(ExportedSymbol {
                    name: name.clone(),
                    kind: ExportedSymbolKind::TypeAlias,
                    signature: format!("type {}", name),
                    params: Vec::new(),
                    returns: None,
                    generic: false,
                    is_macro: false,
                });
                pub_items.push(item.clone());
            }
            Item::ModDecl { inline: Some(inner), pub_: true, .. } => {
                // A `pub mod` re-exports its inner items through the
                // same archive — walk in, but don't emit a symbol for
                // the module itself (HackerOS languages don't share a
                // common notion of nested-module namespacing yet, so
                // consumers see a flat symbol list).
                walk_items(inner, symbols, pub_items);
            }
            _ => {}
        }
    }
}

fn lower_param_type(p: &Param) -> AbiType {
    lower_type_expr(&p.ty).unwrap_or_else(|| AbiType::Opaque {
        struct_name: type_expr_display(&p.ty),
    })
}

/// Lowers an H# `TypeExpr` to the small ABI-stable `AbiType` set shared
/// across HackerOS languages, or `None` when the type has no
/// direct native-ABI representation (references, tuples, closures,
/// unresolved generics, …) — those callers fall back to
/// `AbiType::Opaque` (a pass-through handle) rather than failing the
/// whole export, since a symbol can still be *listed* even if only the
/// producer language can fully make sense of one of its parameters.
fn lower_type_expr(ty: &TypeExpr) -> Option<AbiType> {
    match ty {
        TypeExpr::I8 => Some(AbiType::I8),
        TypeExpr::I16 => Some(AbiType::I16),
        TypeExpr::I32 => Some(AbiType::I32),
        TypeExpr::I64 => Some(AbiType::I64),
        TypeExpr::U8 => Some(AbiType::U8),
        TypeExpr::U16 => Some(AbiType::U16),
        TypeExpr::U32 => Some(AbiType::U32),
        TypeExpr::U64 => Some(AbiType::U64),
        TypeExpr::F32 => Some(AbiType::F32),
        TypeExpr::F64 => Some(AbiType::F64),
        TypeExpr::Bool => Some(AbiType::Bool),
        TypeExpr::Void => Some(AbiType::Void),
        // Strings/byte buffers cross the FFI boundary as a raw pointer
        // by convention (paired with a length parameter per the
        // producer's own calling convention — see `signature` for the
        // human-readable form, and HLIB_FORMAT.md's "ABI stability"
        // section for why this isn't fully automated in v1).
        TypeExpr::String | TypeExpr::Bytes => Some(AbiType::Ptr),
        TypeExpr::I128 | TypeExpr::U128 => Some(AbiType::Opaque { struct_name: type_expr_display(ty) }),
        TypeExpr::Named(name) => Some(AbiType::Opaque { struct_name: name.clone() }),
        // Everything else (generics, tuples, arrays, slices, fn types,
        // refs, optionals) has no single fixed-width native
        // representation every consumer language agrees on — leave it
        // to the AST artifact.
        TypeExpr::Generic(..)
        | TypeExpr::Array(_)
        | TypeExpr::Slice(..)
        | TypeExpr::Tuple(_)
        | TypeExpr::Fn(..)
        | TypeExpr::Optional(_)
        | TypeExpr::Ref(_)
        | TypeExpr::RefMut(_) => None,
    }
}

fn type_expr_display(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::I128 => "i128".to_string(),
        TypeExpr::U128 => "u128".to_string(),
        other => format!("{:?}", other),
    }
}

fn render_fn_signature(name: &str, params: &[Param], ret: &Option<TypeExpr>) -> String {
    let params_str = params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_expr_source(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    match ret {
        Some(r) => format!("fn {}({}) -> {}", name, params_str, type_expr_source(r)),
        None => format!("fn {}({})", name, params_str),
    }
}

/// Renders a `TypeExpr` back to the H# source spelling a human (or the
/// stub generator in `hlib_cmd::cmd_lib_bind`) would write — used only
/// for the informational `signature` string, never parsed back.
fn type_expr_source(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Generic(n, args) => format!(
            "{}<{}>",
            n,
            args.iter().map(type_expr_source).collect::<Vec<_>>().join(", ")
        ),
        TypeExpr::Array(t) => format!("[{}]", type_expr_source(t)),
        TypeExpr::Slice(t, None) => format!("&[{}]", type_expr_source(t)),
        TypeExpr::Slice(t, Some(n)) => format!("[{}; {}]", type_expr_source(t), n),
        TypeExpr::Tuple(items) => format!("({})", items.iter().map(type_expr_source).collect::<Vec<_>>().join(", ")),
        TypeExpr::Fn(params, ret) => format!(
            "fn({}) -> {}",
            params.iter().map(type_expr_source).collect::<Vec<_>>().join(", "),
            type_expr_source(ret)
        ),
        TypeExpr::Optional(t) => format!("{}?", type_expr_source(t)),
        TypeExpr::Ref(t) => format!("&{}", type_expr_source(t)),
        TypeExpr::RefMut(t) => format!("&mut {}", type_expr_source(t)),
        TypeExpr::Void => "void".to_string(),
        TypeExpr::I8 => "i8".to_string(), TypeExpr::I16 => "i16".to_string(),
        TypeExpr::I32 => "i32".to_string(), TypeExpr::I64 => "i64".to_string(), TypeExpr::I128 => "i128".to_string(),
        TypeExpr::U8 => "u8".to_string(), TypeExpr::U16 => "u16".to_string(),
        TypeExpr::U32 => "u32".to_string(), TypeExpr::U64 => "u64".to_string(), TypeExpr::U128 => "u128".to_string(),
        TypeExpr::F32 => "f32".to_string(), TypeExpr::F64 => "f64".to_string(),
        TypeExpr::Bool => "bool".to_string(), TypeExpr::String => "string".to_string(), TypeExpr::Bytes => "bytes".to_string(),
    }
}
