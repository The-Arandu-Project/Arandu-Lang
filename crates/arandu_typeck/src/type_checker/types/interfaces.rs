use rustc_hash::FxHashMap;

use arandu_lexer::Span;
use arandu_parser::{FuncSignature, GenericParam, TypeName, WhereItem};

use super::unify;
use super::{ArType, LowerCtx, TypeId};
use super::{GenericSubst, TypeInterner, build_subst, substitute_type};
use crate::passes::type_checker::TypeChecker;
use crate::{ScopeId, SymbolId, SymbolKind};
use arandu_middle::types::lower::{lower_result_type_ctx, lower_type_expr_ctx};

#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    /// Method name → interned function type **without** receiver
    /// (`ArType::Func` stored as `TypeId`).
    pub methods: Vec<(smol_str::SmolStr, crate::type_checker::types::TypeId)>,
}

/// Collect interface method signatures and per-type-parameter trait constraints.
pub fn collect_interfaces_and_constraints(
    checker: &mut TypeChecker,
    program: &arandu_parser::Program,
) {
    for decl_id in &program.decls {
        let decl = checker.pool.decl(*decl_id);
        use arandu_parser::TopLevelDecl;
        match decl {
            TopLevelDecl::Interface(iface) => collect_interface(checker, iface),
            TopLevelDecl::Struct(s) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(s.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &s.generic_params,
                        &s.where_clause,
                        s.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::Enum(e) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(e.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &e.generic_params,
                        &e.where_clause,
                        e.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::Func(f) => {
                let key = match &f.name {
                    arandu_parser::FuncName::Free { span, .. } => crate::NodeKey::from(*span),
                    arandu_parser::FuncName::Method { span, .. } => crate::NodeKey::from(*span),
                };
                if let Some(sym) = checker.resolved.definitions.get(&key) {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &f.generic_params,
                        &f.where_clause,
                        f.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            TopLevelDecl::TypeAlias(a) => {
                if let Some(sym) = checker
                    .resolved
                    .definitions
                    .get(&crate::NodeKey::from(a.span))
                {
                    let scope = checker.symbols.get(*sym).scope;
                    collect_decl_constraints(
                        checker,
                        &a.generic_params,
                        &[],
                        a.span,
                        Some(*sym),
                        scope,
                    );
                }
            }
            _ => {}
        }
    }
}

fn collect_interface(checker: &mut TypeChecker, decl: &arandu_parser::InterfaceDecl) {
    let Some(iface_sym) = checker
        .resolved
        .definitions
        .get(&crate::NodeKey::from(decl.span))
        .copied()
    else {
        return;
    };
    let iface_scope = checker.symbols.get(iface_sym).scope;
    let type_param_symbols = super::extract_generic_param_symbols(checker, &decl.generic_params);
    if !type_param_symbols.is_empty() {
        checker
            .type_info
            .generic_params
            .insert(iface_sym, std::sync::Arc::new(type_param_symbols.clone()));
    }

    let mut methods = Vec::new();
    for member in &decl.members {
        let sig_ty = lower_func_signature(checker, member, iface_scope);
        let sig_id = checker.intern(sig_ty);
        methods.push((member.name.clone(), sig_id));
    }

    checker
        .type_info
        .interfaces
        .insert(iface_sym, InterfaceInfo { methods });

    collect_decl_constraints(
        checker,
        &decl.generic_params,
        &decl.where_clause,
        decl.span,
        Some(iface_sym),
        iface_scope,
    );
}

fn lower_func_signature(checker: &mut TypeChecker, sig: &FuncSignature, scope: ScopeId) -> ArType {
    let ctx = LowerCtx {
        pool: checker.pool,
        symbols: &checker.symbols,
        scope,
        resolved: &checker.resolved,
    };
    let mut param_types = Vec::new();
    for param in &sig.params {
        let ty = lower_type_expr_ctx(param.ty, &ctx, &mut checker.type_info.type_interner);
        param_types.push(checker.type_info.type_interner.intern(ty));
    }
    let ret = if let Some(result) = &sig.result {
        lower_result_type_ctx(result, &ctx, &mut checker.type_info.type_interner)
    } else {
        ArType::Void
    };
    let ret_id = checker.type_info.type_interner.intern(ret);
    ArType::Func(param_types, ret_id)
}

fn collect_decl_constraints(
    checker: &mut TypeChecker,
    generic_params: &[GenericParam],
    where_clause: &[WhereItem],
    decl_span: Span,
    decl_symbol: Option<SymbolId>,
    scope: ScopeId,
) {
    let param_symbols = if let Some(_decl_sym) = decl_symbol {
        super::extract_generic_param_symbols(checker, generic_params)
    } else {
        Vec::new()
    };

    if !param_symbols.is_empty()
        && let Some(decl_sym) = decl_symbol
    {
        checker
            .type_info
            .generic_params
            .entry(decl_sym)
            .or_insert_with(|| std::sync::Arc::new(param_symbols.clone()));
    }

    let name_to_sym: FxHashMap<smol_str::SmolStr, SymbolId> = generic_params
        .iter()
        .zip(param_symbols.iter())
        .map(|(gp, sym)| (gp.name.clone(), *sym))
        .collect();

    for gp in generic_params {
        let Some(&param_sym) = name_to_sym.get(&gp.name) else {
            continue;
        };
        // T2.1: register default type arg for this type parameter.
        if let Some(def_ty_id) = gp.default {
            let ctx = LowerCtx {
                pool: checker.pool,
                symbols: &checker.symbols,
                scope,
                resolved: &checker.resolved,
            };
            let def_ty = lower_type_expr_ctx(def_ty_id, &ctx, &mut checker.type_info.type_interner);
            let tid = checker.type_info.type_interner.intern(def_ty);
            checker.type_info.generic_defaults.insert(param_sym, tid);
        }
        for constraint in &gp.constraints {
            if let Some(iface_sym) = resolve_interface_constraint(checker, constraint, scope) {
                let entry = checker
                    .type_info
                    .param_constraints
                    .entry(param_sym)
                    .or_insert_with(|| std::sync::Arc::new(Vec::new()));
                std::sync::Arc::make_mut(entry).push(iface_sym);
            }
        }
    }

    for item in where_clause {
        let Some(&param_sym) = name_to_sym.get(&item.name) else {
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                format!(
                    "where clause '{}' does not name a generic parameter of this declaration",
                    item.name
                ),
                item.span,
            ));
            continue;
        };
        for constraint in &item.constraints {
            if let Some(iface_sym) = resolve_interface_constraint(checker, constraint, scope) {
                let entry = checker
                    .type_info
                    .param_constraints
                    .entry(param_sym)
                    .or_insert_with(|| std::sync::Arc::new(Vec::new()));
                std::sync::Arc::make_mut(entry).push(iface_sym);
            }
        }
    }

    let _ = decl_span;
}

fn resolve_interface_constraint(
    checker: &mut TypeChecker,
    type_name: &TypeName,
    _scope: ScopeId,
) -> Option<SymbolId> {
    let key = crate::NodeKey::from(type_name.span);
    let Some(sym) = checker.resolved.type_refs.get(&key).copied() else {
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::N002UndefinedType,
            format!("unknown constraint type '{}'", type_name.path.join(".")),
            type_name.span,
        ));
        return None;
    };
    match checker.symbols.get(sym).kind {
        SymbolKind::Interface => Some(sym),
        _ => {
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                format!(
                    "'{}' is not an interface and cannot be used as a type constraint",
                    type_name.path.join(".")
                ),
                type_name.span,
            ));
            None
        }
    }
}

/// After monomorphic instantiation, verify each type argument satisfies its constraints.
pub(crate) fn check_instantiation_constraints(
    checker: &mut TypeChecker,
    decl_symbol: SymbolId,
    param_symbols: &[SymbolId],
    arg_types: &[ArType],
    span: Span,
) {
    for (param_sym, arg_ty) in param_symbols.iter().zip(arg_types) {
        let constraints = checker.type_info.param_constraints.get(param_sym).cloned();
        let Some(constraints) = constraints else {
            continue;
        };
        for &iface_sym in constraints.iter() {
            if !type_satisfies_interface(checker, arg_ty, iface_sym, span) {
                let iface_name = checker.symbols.get(iface_sym).name.clone();
                let ty_display = arg_ty.display(&checker.symbols, &checker.type_info.type_interner);
                let detail = missing_methods_note(checker, arg_ty, iface_sym);
                // Put the method-level root cause in the primary message (notes are easy to miss).
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T025InterfaceNotSatisfied,
                    format!(
                        "type '{ty_display}' does not satisfy interface '{iface_name}': {detail}"
                    ),
                    span,
                )
                .with_note(detail);
                checker.diagnostics.push(diag);
            }
        }
    }
    let _ = decl_symbol;
}

fn missing_methods_note(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
) -> String {
    let missing = missing_interface_methods(checker, concrete, iface_sym);
    if missing.is_empty() {
        "required method signatures are incompatible".to_string()
    } else {
        format!("missing or incompatible methods: {}", missing.join(", "))
    }
}

pub(crate) fn type_satisfies_interface(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
    _span: Span,
) -> bool {
    // Free type parameters are not concrete types. A param `A: Allocator` is an
    // obligation on instantiations, not something we can structural-check here.
    // Treating `Named(A, [])` as a concrete type caused T025 on every method that
    // restates `A` (Vec, GenArena) even when constraints were well-formed.
    if let ArType::Named(id, args) = concrete
        && args.is_empty()
        && checker.symbols.get(*id).kind == SymbolKind::TypeParam
    {
        // Satisfied iff this param lists `iface_sym` among its constraints.
        if let Some(cs) = checker.type_info.param_constraints.get(id) {
            return cs.contains(&iface_sym);
        }
        return false;
    }

    let Some(iface) = checker.type_info.interfaces.get(&iface_sym) else {
        return false;
    };
    let Some(type_id) = concrete_type_id(concrete) else {
        return false;
    };

    let iface_subst = interface_subst_for_concrete(checker, iface_sym, concrete);

    // We can iterate and borrow interner mutably inside the loop
    // Collect (name, required TypeId) first — avoids holding borrow across resolve.
    let method_specs: Vec<_> = iface.methods.clone();
    for (method, required_id) in method_specs {
        let required = checker.resolve(required_id);
        let required_inst =
            substitute_type(&required, &iface_subst, &checker.type_info.type_interner);
        let Some(provided) = lookup_method_type(checker, type_id, &method) else {
            return false;
        };
        // Interface may list `self: Self`; impl methods always have a receiver.
        // Compare payloads only (TYP.2).
        let required_stripped = strip_interface_receiver(required_inst, checker);
        let provided_stripped = strip_impl_receiver(provided, checker);
        if !method_types_compatible(
            &required_stripped,
            &provided_stripped,
            &checker.type_info.type_interner,
        ) {
            return false;
        }
    }
    true
}

#[cold]
fn missing_interface_methods(
    checker: &mut TypeChecker,
    concrete: &ArType,
    iface_sym: SymbolId,
) -> Vec<String> {
    let Some(iface) = checker.type_info.interfaces.get(&iface_sym) else {
        return vec!["<interface not collected>".to_string()];
    };
    let Some(type_id) = concrete_type_id(concrete) else {
        return vec!["<non-nominal type>".to_string()];
    };

    let iface_subst = interface_subst_for_concrete(checker, iface_sym, concrete);

    let mut missing = Vec::new();
    let method_specs: Vec<_> = iface.methods.clone();
    for (method, required_id) in method_specs {
        let required = checker.resolve(required_id);
        let required_inst =
            substitute_type(&required, &iface_subst, &checker.type_info.type_interner);
        let Some(provided) = lookup_method_type(checker, type_id, &method) else {
            let mut similar = Vec::new();
            if let Some(methods) = checker.symbols.associated_members.get(&type_id) {
                let max_distance = if method.len() <= 4 { 2 } else { 3 };
                for prov_name in methods.keys() {
                    let dist = if prov_name.to_lowercase() == method.to_lowercase() {
                        0
                    } else {
                        strsim::levenshtein(method.as_str(), prov_name)
                    };
                    if dist <= max_distance {
                        similar.push(prov_name.to_string());
                    }
                }
            }
            if !similar.is_empty() {
                missing.push(format!(
                    "{method} (did you mean `{}`?)",
                    similar.join("`, `")
                ));
            } else {
                missing.push(method.to_string());
            }
            continue;
        };
        let required_stripped = strip_interface_receiver(required_inst, checker);
        let provided_stripped = strip_impl_receiver(provided, checker);
        if !method_types_compatible(
            &required_stripped,
            &provided_stripped,
            &checker.type_info.type_interner,
        ) {
            missing.push(format!("{method} (signature mismatch)"));
        }
    }
    missing
}

fn concrete_type_id(ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(id, _) => Some(*id),
        _ => None,
    }
}

fn interface_subst_for_concrete(
    checker: &TypeChecker,
    iface_sym: SymbolId,
    concrete: &ArType,
) -> GenericSubst {
    let Some(iface_params) = checker.type_info.generic_params.get(&iface_sym) else {
        return GenericSubst::default();
    };
    if iface_params.is_empty() {
        return GenericSubst::default();
    }
    if let ArType::Named(_, args) = concrete
        && args.len() == iface_params.len()
    {
        let resolved_args: Vec<ArType> = args
            .iter()
            .map(|&a| checker.type_info.type_interner.resolve(a))
            .collect();
        return build_subst(iface_params, &resolved_args);
    }
    if iface_params.len() == 1 {
        return build_subst(iface_params, std::slice::from_ref(concrete));
    }
    GenericSubst::default()
}

fn lookup_method_type(checker: &TypeChecker, type_id: SymbolId, method: &str) -> Option<ArType> {
    let sym = checker.symbols.lookup_associated_member(type_id, method)?;
    checker.decl_type(sym)
}

/// Drop leading `Self` / `&Self` / `&mut Self` from an interface method formal.
fn strip_interface_receiver(ty: ArType, checker: &TypeChecker<'_>) -> ArType {
    let ArType::Func(params, ret) = ty else {
        return ty;
    };
    if params.is_empty() {
        return ArType::Func(params, ret);
    }
    if is_self_type(checker, params[0]) {
        ArType::Func(params[1..].to_vec(), ret)
    } else {
        ArType::Func(params, ret)
    }
}

/// Drop the concrete method receiver (`T` / `&T` / `&mut T`) — always first formal.
fn strip_impl_receiver(ty: ArType, checker: &TypeChecker<'_>) -> ArType {
    let ArType::Func(params, ret) = ty else {
        return ty;
    };
    if params.is_empty() {
        return ArType::Func(params, ret);
    }
    // Always peel first: impl methods are `Type.m(self, …)`. Also peel Self
    // if somehow present.
    let _ = checker;
    ArType::Func(params[1..].to_vec(), ret)
}

fn is_self_type(checker: &TypeChecker<'_>, tid: TypeId) -> bool {
    match checker.resolve(tid) {
        ArType::Named(id, _) => checker.symbols.get(id).name == "Self",
        ArType::Ref(inner) | ArType::RefMut(inner) => is_self_type(checker, inner),
        _ => false,
    }
}

fn method_types_compatible(required: &ArType, provided: &ArType, interner: &TypeInterner) -> bool {
    match (required, provided) {
        (ArType::Func(req_params, req_ret), ArType::Func(prov_params, prov_ret)) => {
            if req_params.len() != prov_params.len() {
                return false;
            }
            req_params.iter().zip(prov_params.iter()).all(|(&a, &b)| {
                if a == b {
                    return true;
                }
                let ty_a = interner.resolve(a);
                let ty_b = interner.resolve(b);
                unify(&ty_a, &ty_b, interner)
            }) && (*req_ret == *prov_ret || {
                let ty_a = interner.resolve(*req_ret);
                let ty_b = interner.resolve(*prov_ret);
                unify(&ty_a, &ty_b, interner)
            })
        }
        _ => unify(required, provided, interner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::type_checker::ResolvedNames;
    use crate::type_checker::types::Primitive;
    use arandu_lexer::Span;
    use arandu_middle::symbol_table::{Symbol, SymbolTable};
    use arandu_middle::{ScopeId, SymbolId, SymbolKind};
    use arandu_parser::Program;
    use arandu_parser::ast_pool::AstPool;

    #[test]
    fn test_collect_interfaces_and_constraints_empty() {
        let pool = AstPool::default();
        let symbols = SymbolTable::new(0);
        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let program = Program {
            span: Span::new(0, 0, 0),
            module: None,
            imports: Vec::new(),
            decls: Vec::new(),
            docs: Vec::new(),
            pool: AstPool::default(),
        };

        collect_interfaces_and_constraints(&mut checker, &program);
        assert!(checker.type_info.interfaces.is_empty());
    }

    #[test]
    fn test_type_satisfies_interface() {
        let pool = AstPool::default();
        let mut symbols = SymbolTable::new(0);

        let iface_sym = SymbolId::new(1, 0);
        let iface_symbol = Symbol {
            id: iface_sym,
            name: "Reader".into(),
            kind: SymbolKind::Interface,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
        };
        symbols.register_imported_symbol(iface_symbol);

        let struct_sym_id = SymbolId::new(1, 1);
        let struct_symbol = Symbol {
            id: struct_sym_id,
            name: "MyStruct".into(),
            kind: SymbolKind::Struct,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
        };
        symbols.register_imported_symbol(struct_symbol);

        let self_sym_id = SymbolId::new(1, 99);
        let self_symbol = Symbol {
            id: self_sym_id,
            name: "Self".into(),
            kind: SymbolKind::TypeParam,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
        };
        symbols.register_imported_symbol(self_symbol);

        let method_sym_id = SymbolId::new(1, 2);
        let method_symbol = Symbol {
            id: method_sym_id,
            name: "read".into(),
            kind: SymbolKind::Func,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            is_public: true,
        };
        symbols.register_imported_symbol(method_symbol);

        let mut associated = rustc_hash::FxHashMap::default();
        associated.insert("read".into(), method_sym_id);
        symbols.associated_members.insert(struct_sym_id, associated);

        let resolved = ResolvedNames::default();
        let mut checker = TypeChecker::new(
            symbols,
            resolved,
            Vec::new(),
            &pool,
            crate::type_checker::TargetInfo { pointer_width: 64 },
        );

        let self_type_id = checker.intern(ArType::Named(struct_sym_id, Vec::new()));
        let self_interface_type_id = checker.intern(ArType::Named(self_sym_id, Vec::new()));
        let req_method_type = ArType::Func(
            vec![self_interface_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
        );
        let prov_method_type = ArType::Func(
            vec![self_type_id],
            checker.intern(ArType::Primitive(Primitive::Int)),
        );
        let req_method_type_id = checker.intern(req_method_type);
        let prov_method_type_id = checker.intern(prov_method_type);

        checker
            .type_info
            .decl_types
            .insert(method_sym_id, prov_method_type_id);

        let iface_info = InterfaceInfo {
            methods: vec![("read".into(), req_method_type_id)],
        };
        checker.type_info.interfaces.insert(iface_sym, iface_info);

        let concrete = ArType::Named(struct_sym_id, Vec::new());
        assert!(type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            Span::new(0, 0, 0)
        ));

        checker.symbols.associated_members.clear();
        assert!(!type_satisfies_interface(
            &mut checker,
            &concrete,
            iface_sym,
            Span::new(0, 0, 0)
        ));

        let missing = missing_interface_methods(&mut checker, &concrete, iface_sym);
        assert_eq!(missing, vec!["read".to_string()]);
    }
}
