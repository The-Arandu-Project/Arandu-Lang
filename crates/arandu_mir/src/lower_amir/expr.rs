use super::{LowerCtx, amir_unsupported};
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt, TempId};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::ops::BinaryOp;
use crate::passes::type_checker::types::{ArType, Primitive, is_option_type, result_ok_err_id};
use crate::{SymbolKind, SymbolTable};

use crate::hir::{HirExpr, HirExprId, HirExprKind, ResultCtorVariant};

fn resolve_method_target(
    callee: &HirExpr,
    pool: &crate::hir::HirPool,
    symbols: &SymbolTable,
    interner: &crate::types::TypeInterner,
) -> Option<crate::SymbolId> {
    let (base_id, field) = match &callee.kind {
        HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
            (*base, field)
        }
        _ => return None,
    };

    let base_expr = pool.expr(base_id);
    // Peel Nullable / & / &mut / ptr so `shared self: &T` still resolves methods
    // (same family as typeck synth_method_call + PROMOTE-L1 interface via T).
    let mut base_ty = interner.resolve(base_expr.ty);
    for _ in 0..4 {
        base_ty = match base_ty {
            ArType::Nullable(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Ptr(inner) => interner.resolve(inner),
            other => other,
        };
        if matches!(
            base_ty,
            ArType::Named(_, _) | ArType::Result(_, _) | ArType::Option(_)
        ) {
            break;
        }
    }
    // Named receivers use their type SymbolId; builtin Result/Option methods
    // live on the prelude type symbols (linked by import re-index).
    let type_id = match base_ty {
        ArType::Named(id, _) => Some(id),
        ArType::Result(_, _) => symbols.lookup_type(symbols.global_scope(), "Result"),
        ArType::Option(_) => symbols.lookup_type(symbols.global_scope(), "Option"),
        _ => None,
    }?;

    symbols.lookup_associated_member(type_id, field)
}

impl LowerCtx<'_> {
    /// If `src_ty` is formatable and not already `str`, emit `ToStr` into a
    /// fresh `str` temp. Identity for `str`; other types left unchanged (typeck
    /// should already have rejected non-formatables at str sites).
    pub(crate) fn maybe_to_str(
        &mut self,
        op: AmirOperand,
        src_ty: crate::types::TypeId,
    ) -> Result<AmirOperand, Diagnostic> {
        if self.tc.type_info.type_interner.is_error(src_ty) {
            return Ok(op);
        }
        let needs = self.tc.type_info.type_interner.with_type(src_ty, |t| {
            !matches!(t, ArType::Primitive(Primitive::Str)) && t.is_to_str_v01()
        });
        if !needs {
            return Ok(op);
        }
        let src_ty_id = src_ty;
        let dest = self.new_temp(ArType::Primitive(Primitive::Str));
        self.emit_assign_temp(
            dest,
            AmirRvalue::ToStr {
                value: op,
                src_ty: src_ty_id,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_expr(
        &mut self,
        expr_id: HirExprId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let expr = self.hir.pool.expr(expr_id);
        self.current_span = expr.span;
        match &expr.kind {
            HirExprKind::Int(v) => {
                // Move SmolStr into the pool when the expr is consumed by ref via clone of short str.
                let op = AmirOperand::Constant(self.intern_literal_int(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Float(v) => {
                let op = AmirOperand::Constant(self.intern_literal_float(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Bool(v) => {
                let op = AmirOperand::Constant(AmirConstant::Bool(*v));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Str(v) => {
                let op = AmirOperand::Constant(self.intern_literal_str(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::StringInterp { parts } => {
                let mut part_ops = Vec::with_capacity(parts.len());
                for part in parts {
                    let op = match part {
                        arandu_middle::hir::HirStringPart::Text(t) => {
                            AmirOperand::Constant(self.intern_literal_str(t.clone()))
                        }
                        arandu_middle::hir::HirStringPart::Expr(e) => {
                            let part_expr = self.hir.pool.expr(*e);
                            let part_op = self.lower_expr(*e, None, symbols)?;
                            self.maybe_to_str(part_op, part_expr.ty)?
                        }
                    };
                    part_ops.push(op);
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::StringInterp { parts: part_ops });
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::ToStr { value } => {
                let value_expr = self.hir.pool.expr(*value);
                let op = self.lower_expr(*value, None, symbols)?;
                // Always materialize ToStr (even for `str` identity is a no-op in maybe_to_str).
                let str_op = self.maybe_to_str(op, value_expr.ty)?;
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(str_op));
                    Ok(AmirOperand::Copy(dest))
                } else {
                    Ok(str_op)
                }
            }
            HirExprKind::Char(v) => {
                let op = AmirOperand::Constant(self.intern_literal_char(v.clone()));
                if let Some(dest) = target {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Nil => {
                let op = if self.with_ty(expr.ty, is_option_type) {
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: None,
                        },
                    );
                    AmirOperand::Copy(dest)
                } else {
                    AmirOperand::Constant(AmirConstant::Nil)
                };
                if let (Some(dest), false) = (target, self.with_ty(expr.ty, is_option_type)) {
                    self.emit_assign_temp(dest, AmirRvalue::Use(op));
                }
                Ok(op)
            }
            HirExprKind::Path { symbol } => {
                // Derive the parent enum SymbolId from the expression's resolved type.
                // The type checker always resolves an enum-variant expression to
                // ArType::Named(enum_sym, []), so we can use that as a filter anchor
                // instead of doing a global name-based scan — which would silently
                // pick the wrong discriminant when two enums share a variant name.
                let enum_sym_from_ty = match self.resolve_ty(expr.ty) {
                    ArType::Named(id, _) => Some(id),
                    _ => None,
                };
                let op: AmirOperand = if let Some(&local_id) = self.symbol_map.get(symbol) {
                    Ok::<AmirOperand, Diagnostic>(self.read_variable_source(local_id)?)
                } else if let Some(&tag) =
                    self.tc.type_info.enum_variant_tags.get(symbol).or_else(|| {
                        // Fallback: find the canonical variant SymbolId whose parent enum
                        // matches the type we already know this expression has, then look up
                        // its tag. No string comparison needed — anchored by SymbolId.
                        let enum_id = enum_sym_from_ty?;
                        self.tc
                            .type_info
                            .enum_variants
                            .iter()
                            .find(|&(v_sym, (parent_sym, _))| {
                                *parent_sym == enum_id
                                    && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                    && {
                                        // Name must match (bare suffix of the lookup symbol vs
                                        // bare suffix of the registered variant symbol).
                                        let lookup_bare = symbols
                                            .get(*symbol)
                                            .name
                                            .rsplit('.')
                                            .next()
                                            .unwrap_or("");
                                        let reg_bare = symbols
                                            .get(*v_sym)
                                            .name
                                            .rsplit('.')
                                            .next()
                                            .unwrap_or("");
                                        lookup_bare == reg_bare
                                    }
                            })
                            .and_then(|(v_sym, _)| self.tc.type_info.enum_variant_tags.get(v_sym))
                    })
                {
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: tag,
                            payload: None,
                        },
                    );
                    Ok(AmirOperand::Copy(dest))
                } else {
                    let sym = symbols.get(*symbol);
                    Ok(match sym.kind {
                        SymbolKind::Func
                        | SymbolKind::ExternFunc
                        | SymbolKind::AssociatedFunc
                        | SymbolKind::NamespaceMember => AmirOperand::FunctionRef(*symbol),
                        _ => AmirOperand::GlobalRef(*symbol),
                    })
                }?;
                if let Some(dest) = target {
                    let already_assigned = self.tc.type_info.enum_variant_tags.contains_key(symbol)
                        || enum_sym_from_ty.is_some_and(|enum_id| {
                            let lookup_bare =
                                symbols.get(*symbol).name.rsplit('.').next().unwrap_or("");
                            self.tc
                                .type_info
                                .enum_variants
                                .iter()
                                .any(|(v_sym, (parent, _))| {
                                    *parent == enum_id
                                        && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                            == lookup_bare
                                        && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                })
                        });
                    if !already_assigned {
                        let rhs = self.consume_operand(op)?;
                        self.emit_assign_temp(dest, AmirRvalue::Use(rhs));
                    }
                }
                Ok(op)
            }
            HirExprKind::TypePath {
                type_symbol,
                member_symbol,
            } => {
                let op: AmirOperand = if let Some(&local_id) = self.symbol_map.get(member_symbol) {
                    Ok::<AmirOperand, Diagnostic>(self.read_variable_source(local_id)?)
                } else if let Some(&tag) = self
                    .tc
                    .type_info
                    .enum_variant_tags
                    .get(member_symbol)
                    .or_else(|| {
                        // Filter by the enum type that the parser already resolved (type_symbol).
                        // This eliminates cross-enum collisions for identically-named variants.
                        let lookup_bare = symbols
                            .get(*member_symbol)
                            .name
                            .rsplit('.')
                            .next()
                            .unwrap_or("");
                        self.tc
                            .type_info
                            .enum_variants
                            .iter()
                            .find(|&(v_sym, (parent_sym, _))| {
                                *parent_sym == *type_symbol
                                    && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                        == lookup_bare
                                    && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                            })
                            .and_then(|(v_sym, _)| self.tc.type_info.enum_variant_tags.get(v_sym))
                    })
                {
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: tag,
                            payload: None,
                        },
                    );
                    Ok(AmirOperand::Copy(dest))
                } else {
                    let sym = symbols.get(*member_symbol);
                    Ok(match sym.kind {
                        SymbolKind::Func
                        | SymbolKind::ExternFunc
                        | SymbolKind::AssociatedFunc
                        | SymbolKind::NamespaceMember => AmirOperand::FunctionRef(*member_symbol),
                        _ => AmirOperand::GlobalRef(*member_symbol),
                    })
                }?;
                if let Some(dest) = target {
                    let lookup_bare = symbols
                        .get(*member_symbol)
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or("");
                    let already_assigned =
                        self.tc
                            .type_info
                            .enum_variant_tags
                            .contains_key(member_symbol)
                            || self.tc.type_info.enum_variants.iter().any(
                                |(v_sym, (parent, _))| {
                                    *parent == *type_symbol
                                        && symbols.get(*v_sym).name.rsplit('.').next().unwrap_or("")
                                            == lookup_bare
                                        && self.tc.type_info.enum_variant_tags.contains_key(v_sym)
                                },
                            );
                    if !already_assigned {
                        let rhs = self.consume_operand(op)?;
                        self.emit_assign_temp(dest, AmirRvalue::Use(rhs));
                    }
                }
                Ok(op)
            }

            HirExprKind::Generic { callee, args } => {
                // mem.sizeOf<T>() / mem.alignOf<T>() — fold to target layout constants so
                // the JIT never needs a runtime `fn@sizeOf` symbol (L6.1 mem intrinsics).
                if let Some(op) = self
                    .try_lower_mem_size_align_intrinsic(*callee, args, expr.ty, target, symbols)?
                {
                    return Ok(op);
                }
                self.lower_expr(*callee, target, symbols)
            }
            HirExprKind::Alloc { expr: inner } => {
                let inner_op = self.lower_expr(*inner, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Alloc(inner_op));
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Binary { op, left, right } => {
                self.lower_binary(*op, *left, *right, expr.ty, target, symbols)
            }
            HirExprKind::Unary { op, expr: sub_expr } => {
                self.lower_unary(*op, *sub_expr, expr.ty, target, symbols)
            }
            HirExprKind::Field { base, field } => {
                self.lower_field(*base, field.as_str(), expr.ty, target, symbols)
            }
            HirExprKind::Index { base, index } => {
                self.lower_index(*base, *index, expr.ty, target, symbols)
            }
            HirExprKind::Array { items } => {
                let items_slice = self.hir.pool.expr_list(*items);
                let mut item_ops = Vec::with_capacity(items_slice.len());
                for &item in items_slice {
                    item_ops.push(self.lower_expr(item, None, symbols)?);
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Array { items: item_ops });
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Call { callee, args, .. } => {
                let callee_expr = self.hir.pool.expr(*callee);

                // `std.testing.blackBox<T>(value)` is a compiler-recognized
                // identity barrier. Lower it before generic call expansion so
                // DCE and both backends see the explicit AMIR contract.
                let black_box_symbol = match &callee_expr.kind {
                    HirExprKind::Path { symbol } => Some(*symbol),
                    HirExprKind::Generic { callee, .. } => {
                        match &self.hir.pool.expr(*callee).kind {
                            HirExprKind::Path { symbol } => Some(*symbol),
                            HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                            _ => None,
                        }
                    }
                    HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                    _ => None,
                };
                if black_box_symbol.is_some_and(|symbol| {
                    let name = symbols.get(symbol).name.as_str();
                    arandu_middle::IntrinsicKind::from_name(name)
                        == Some(arandu_middle::IntrinsicKind::BlackBox)
                }) {
                    let args_slice = self.hir.pool.expr_list(*args);
                    if let Some(&argument) = args_slice.first() {
                        let value = self.lower_expr(argument, None, symbols)?;
                        let value_ty = self.hir.pool.expr(argument).ty;
                        let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                        self.emit_assign_temp(dest, AmirRvalue::BlackBox { value, value_ty });
                        return Ok(AmirOperand::Copy(dest));
                    }
                }

                // `mem.sizeOf<T>()` is Call(Generic(sizeOf, [T]), []) — fold before
                // treating Generic as a callable value (L6.1).
                if let HirExprKind::Generic {
                    callee: g_cal,
                    args: type_args,
                } = &callee_expr.kind
                    && let Some(op) = self.try_lower_mem_size_align_intrinsic(
                        *g_cal, type_args, expr.ty, target, symbols,
                    )?
                {
                    return Ok(op);
                }

                let mut is_enum_ctor = None;
                match &callee_expr.kind {
                    HirExprKind::Path { symbol } => {
                        let enum_sym_from_ty = match self.resolve_ty(callee_expr.ty) {
                            ArType::Named(id, _) => Some(id),
                            ArType::Func(_, ret) => {
                                match self.tc.type_info.type_interner.resolve(ret) {
                                    ArType::Named(id, _) => Some(id),
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some(&tag) =
                            self.tc.type_info.enum_variant_tags.get(symbol).or_else(|| {
                                let enum_id = enum_sym_from_ty?;
                                self.tc
                                    .type_info
                                    .enum_variants
                                    .iter()
                                    .find(|&(v_sym, (parent_sym, _))| {
                                        *parent_sym == enum_id
                                            && self
                                                .tc
                                                .type_info
                                                .enum_variant_tags
                                                .contains_key(v_sym)
                                            && {
                                                let lookup_bare = symbols
                                                    .get(*symbol)
                                                    .name
                                                    .rsplit('.')
                                                    .next()
                                                    .unwrap_or("");
                                                let reg_bare = symbols
                                                    .get(*v_sym)
                                                    .name
                                                    .rsplit('.')
                                                    .next()
                                                    .unwrap_or("");
                                                lookup_bare == reg_bare
                                            }
                                    })
                                    .and_then(|(v_sym, _)| {
                                        self.tc.type_info.enum_variant_tags.get(v_sym)
                                    })
                            })
                        {
                            is_enum_ctor = Some(tag);
                        }
                    }
                    HirExprKind::TypePath {
                        type_symbol,
                        member_symbol,
                    } => {
                        if let Some(&tag) = self
                            .tc
                            .type_info
                            .enum_variant_tags
                            .get(member_symbol)
                            .or_else(|| {
                                let lookup_bare = symbols
                                    .get(*member_symbol)
                                    .name
                                    .rsplit('.')
                                    .next()
                                    .unwrap_or("");
                                self.tc
                                    .type_info
                                    .enum_variants
                                    .iter()
                                    .find(|&(v_sym, (parent_sym, _))| {
                                        *parent_sym == *type_symbol
                                            && symbols
                                                .get(*v_sym)
                                                .name
                                                .rsplit('.')
                                                .next()
                                                .unwrap_or("")
                                                == lookup_bare
                                            && self
                                                .tc
                                                .type_info
                                                .enum_variant_tags
                                                .contains_key(v_sym)
                                    })
                                    .and_then(|(v_sym, _)| {
                                        self.tc.type_info.enum_variant_tags.get(v_sym)
                                    })
                            })
                        {
                            is_enum_ctor = Some(tag);
                        }
                    }
                    _ => {}
                }

                if let Some(tag) = is_enum_ctor {
                    let args_slice = self.hir.pool.expr_list(*args);
                    let payload_op = match args_slice.len() {
                        0 => None,
                        1 => Some(self.lower_expr(args_slice[0], None, symbols)?),
                        _ => {
                            let mut item_ops = Vec::with_capacity(args_slice.len());
                            for &arg in args_slice {
                                item_ops.push(self.lower_expr(arg, None, symbols)?);
                            }
                            let param_tys = match self.resolve_ty(callee_expr.ty) {
                                ArType::Func(params, _) => params,
                                _ => vec![],
                            };
                            let tuple_ty = ArType::Tuple(param_tys);
                            let dest_tuple = self.new_temp(tuple_ty);
                            self.emit_assign_temp(
                                dest_tuple,
                                AmirRvalue::Tuple { items: item_ops },
                            );
                            Some(AmirOperand::Copy(dest_tuple))
                        }
                    };
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: tag,
                            payload: payload_op,
                        },
                    );
                    return Ok(AmirOperand::Copy(dest));
                }

                let method_target = resolve_method_target(
                    callee_expr,
                    &self.hir.pool,
                    symbols,
                    &self.tc.type_info.type_interner,
                );
                let callee_symbol = method_target.or(match &callee_expr.kind {
                    HirExprKind::Path { symbol } => Some(*symbol),
                    HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                    HirExprKind::Generic { callee, .. } => {
                        match &self.hir.pool.expr(*callee).kind {
                            HirExprKind::Path { symbol } => Some(*symbol),
                            HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                            _ => None,
                        }
                    }
                    _ => None,
                });
                if let Some(callee_symbol) = callee_symbol {
                    let callee_name = symbols.get(callee_symbol).name.as_str();
                    let is_testing_operation = [
                        "expect",
                        "expectEqualInt",
                        "expectEqualFloat",
                        "expectEqualBool",
                        "expectEqualStr",
                        "fail",
                        "skip",
                        "log",
                        "tempDir",
                    ]
                    .iter()
                    .any(|operation| {
                        callee_name == *operation || callee_name.ends_with(&format!(".{operation}"))
                    });
                    if is_testing_operation
                        && let Some(set_span_id) = symbols
                            .iter()
                            .find(|symbol| symbol.name == "ar_test_set_span")
                            .map(|symbol| symbol.id)
                    {
                        let file_id = AmirOperand::Constant(
                            self.intern_literal_int(expr.span.file_id.to_string()),
                        );
                        let start = AmirOperand::Constant(
                            self.intern_literal_int(expr.span.start.to_string()),
                        );
                        let end = AmirOperand::Constant(
                            self.intern_literal_int(expr.span.end.to_string()),
                        );
                        self.push_stmt(AmirStmt::Call {
                            lhs: None,
                            callee: AmirOperand::FunctionRef(set_span_id),
                            args: smallvec::smallvec![file_id, start, end],
                            return_borrow: None,
                        });
                    }
                }
                let callee_op = if let Some(method_symbol) = method_target {
                    AmirOperand::FunctionRef(method_symbol)
                } else {
                    self.lower_expr(*callee, None, symbols)?
                };
                let args_slice = self.hir.pool.expr_list(*args);
                // Method calls: HIR already includes the receiver as arg 0 when
                // typeck rewrites `obj.m(a)` → Call(Field(m), [obj, a]). Only inject
                // `base` when args are short of the formal arity (legacy / incomplete HIR).
                let formal_params: Vec<ArType> = match self.resolve_ty(callee_expr.ty) {
                    ArType::Func(params, _) => {
                        params.iter().map(|&id| self.resolve_ty(id)).collect()
                    }
                    _ => Vec::new(),
                };
                let mut arg_ops = Vec::with_capacity(args_slice.len() + 1);
                let inject_receiver = method_target.is_some()
                    && !formal_params.is_empty()
                    && args_slice.len() < formal_params.len();
                // Arg consume modes from the post-mono `CalleeArgModes` table (O(1)).
                let callee_for_modes = callee_symbol.unwrap_or(crate::SymbolId::DUMMY);
                if inject_receiver
                    && let HirExprKind::Field { base, .. } | HirExprKind::SafeField { base, .. } =
                        &callee_expr.kind
                {
                    let formal0 = formal_params.first();
                    arg_ops.push(self.lower_call_arg(
                        *base,
                        0,
                        callee_for_modes,
                        formal0,
                        symbols,
                    )?);
                }
                let arg_param_offset = if inject_receiver { 1 } else { 0 };
                for (i, &arg) in args_slice.iter().enumerate() {
                    let arg_expr = self.hir.pool.expr(arg);
                    let formal_i = i + arg_param_offset;
                    let formal = formal_params.get(formal_i);
                    let arg_op =
                        self.lower_call_arg(arg, formal_i, callee_for_modes, formal, symbols)?;
                    let arg_op = if let Some(param_ty) = formal {
                        if matches!(param_ty, ArType::Primitive(Primitive::Str)) {
                            self.maybe_to_str(arg_op, arg_expr.ty)?
                        } else {
                            arg_op
                        }
                    } else {
                        arg_op
                    };
                    arg_ops.push(arg_op);
                }
                let dest = if self.with_ty(expr.ty, |t| matches!(t, ArType::Void)) {
                    None
                } else {
                    Some(target.unwrap_or_else(|| self.new_temp_id(expr.ty)))
                };
                if let Some(symbol) = callee_symbol {
                    let name = symbols.get(symbol).name.as_str();
                    let kind = arandu_middle::IntrinsicKind::from_name(name);
                    let intrinsic = match (kind, arg_ops.as_slice()) {
                        (Some(arandu_middle::IntrinsicKind::SliceFromRaw), [owner, data, len]) => {
                            Some(AmirRvalue::SliceView {
                                owner: *owner,
                                data: *data,
                                len: *len,
                            })
                        }
                        (
                            Some(arandu_middle::IntrinsicKind::SliceSubslice),
                            [slice, start, len],
                        ) => Some(AmirRvalue::SliceSubslice {
                            slice: *slice,
                            start: *start,
                            len: *len,
                        }),
                        (Some(arandu_middle::IntrinsicKind::SliceLen), [slice]) => {
                            Some(AmirRvalue::Len(*slice))
                        }
                        (Some(arandu_middle::IntrinsicKind::StrView), [owner]) => {
                            Some(AmirRvalue::StrView { owner: *owner })
                        }
                        _ => None,
                    };
                    if let (Some(intrinsic), Some(dest)) = (intrinsic, dest) {
                        self.emit_assign_temp(dest, intrinsic);
                        return Ok(AmirOperand::Copy(dest));
                    }
                }
                self.push_stmt(AmirStmt::Call {
                    lhs: dest,
                    callee: callee_op,
                    args: arg_ops.into(),
                    return_borrow: callee_symbol
                        .and_then(|symbol| self.tc.type_info.return_borrow_summaries.get(&symbol))
                        .cloned(),
                });
                Ok(dest.map_or(AmirOperand::Constant(AmirConstant::Nil), AmirOperand::Copy))
            }
            HirExprKind::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let fields_slice = self.hir.pool.field_inits_list(*fields);
                let mut field_ops = Vec::with_capacity(fields_slice.len());
                for f in fields_slice {
                    field_ops.push((f.name.clone(), self.lower_expr(f.value, None, symbols)?));
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::StructLiteral {
                        struct_symbol: *struct_symbol,
                        fields: field_ops,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let cond_op = self.lower_condition(condition, symbols)?;
                if self.builder.current_block.is_none() {
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                    return Ok(AmirOperand::Copy(dest));
                }
                let bb_then = self.new_block();
                let bb_else = self.new_block();
                let bb_join = self.new_block();

                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));

                self.set_bool_branch(cond_op, bb_then, bb_else);
                self.seal_block(bb_then);
                self.seal_block(bb_else);

                // Then branch
                self.builder.current_block = Some(bb_then);
                self.lower_block_as_expr(*then_block, Some(dest), symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                // Else branch
                self.builder.current_block = Some(bb_else);
                self.lower_block_as_expr(*else_block, Some(dest), symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                // Join
                self.seal_block(bb_join);
                self.builder.current_block = Some(bb_join);
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Cast { expr: sub_expr, .. } => {
                let sub_op = self.lower_expr(*sub_expr, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                self.emit_assign_temp(dest, AmirRvalue::Use(sub_op));
                Ok(AmirOperand::Copy(dest))
            }

            HirExprKind::Match { value, arms } => {
                self.lower_match(*value, arms, target, expr.ty, symbols)
            }
            HirExprKind::ResultCtor { variant, value } => {
                let val_op = self.lower_expr(*value, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                match variant {
                    ResultCtorVariant::Ok => {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 0,
                                payload: Some(val_op),
                            },
                        );
                    }
                    ResultCtorVariant::Err => {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 1,
                                payload: Some(val_op),
                            },
                        );
                    }
                    ResultCtorVariant::Some => {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 1,
                                payload: Some(val_op),
                            },
                        );
                    }
                    // Option.None = tag 0, no payload (Some is tag 1).
                    ResultCtorVariant::None => {
                        let _ = val_op;
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 0,
                                payload: None,
                            },
                        );
                    }
                    // A3.6: Poll.Ready = tag 0 + payload; Poll.Pending = tag 1, no payload.
                    ResultCtorVariant::PollReady => {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 0,
                                payload: Some(val_op),
                            },
                        );
                    }
                    ResultCtorVariant::PollPending => {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::EnumConstruct {
                                variant_tag: 1,
                                payload: None,
                            },
                        );
                    }
                }
                Ok(AmirOperand::Copy(dest))
            }

            HirExprKind::Try { expr: inner } => {
                let inner_expr = self.hir.pool.expr(*inner);
                if result_ok_err_id(inner_expr.ty, &self.tc.type_info.type_interner).is_some() {
                    self.lower_try_result(*inner, target, expr.ty, symbols)
                } else if self.with_ty(inner_expr.ty, is_option_type) {
                    self.lower_try_option(*inner, target, expr.ty, symbols)
                } else if self.with_ty(inner_expr.ty, |t| matches!(t, ArType::Nullable(_))) {
                    self.lower_try_nullable(*inner, target, expr.ty, symbols)
                } else {
                    self.lower_try_result(*inner, target, expr.ty, symbols)
                }
            }
            HirExprKind::SafeField { base, field } => {
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                let base_expr = self.hir.pool.expr(*base);
                let base_is_option = self.with_ty(base_expr.ty, is_option_type);
                let base_op = self.lower_expr(*base, None, symbols)?;
                if self.builder.current_block.is_none() {
                    return Ok(AmirOperand::Copy(dest));
                }

                let bb_null = self.new_block();
                let bb_access = self.new_block();
                let bb_join = self.new_block();

                if base_is_option {
                    // Option: tag 0 is None, tag 1 is Some
                    let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
                    self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: base_op });
                    let zero_lit = self.intern_literal_int("0");
                    let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        cond_tmp,
                        AmirRvalue::Binary {
                            op: BinaryOp::Equal,
                            left: AmirOperand::Copy(tag_tmp),
                            right: AmirOperand::Constant(zero_lit),
                        },
                    );
                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
                    self.seal_block(bb_null);
                    self.seal_block(bb_access);

                    self.builder.current_block = Some(bb_null);
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: 0,
                            payload: None,
                        },
                    );
                    self.emit_goto(bb_join);

                    self.builder.current_block = Some(bb_access);
                    let payload_ty = match self.resolve_ty(base_expr.ty) {
                        ArType::Option(inner) => inner,
                        _ => base_expr.ty,
                    };
                    let payload_tmp = self.new_temp_id(payload_ty);
                    self.lower_result_ok_field(base_op, payload_tmp);
                    let payload_resolved = self.resolve_ty(payload_ty);
                    let field_idx = self.resolve_field_index(&payload_resolved, field);
                    let field_val_tmp = self.new_temp(ArType::Error); // will be used as payload
                    self.emit_assign_temp(
                        field_val_tmp,
                        AmirRvalue::FieldAccess {
                            base: AmirOperand::Copy(payload_tmp),
                            field: field_idx,
                        },
                    );
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::EnumConstruct {
                            variant_tag: 1,
                            payload: Some(AmirOperand::Copy(field_val_tmp)),
                        },
                    );
                    self.emit_goto(bb_join);
                } else {
                    let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        cond_tmp,
                        AmirRvalue::Binary {
                            op: BinaryOp::Equal,
                            left: base_op,
                            right: AmirOperand::Constant(AmirConstant::Nil),
                        },
                    );

                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
                    self.seal_block(bb_null);
                    self.seal_block(bb_access);

                    self.builder.current_block = Some(bb_null);
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                    );
                    self.emit_goto(bb_join);

                    self.builder.current_block = Some(bb_access);
                    let base_tmp = self.new_temp_id(base_expr.ty);
                    self.emit_assign_temp(base_tmp, AmirRvalue::Use(base_op));
                    let base_ty = self.resolve_ty(base_expr.ty);
                    let field_idx = self.resolve_field_index(&base_ty, field);
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::FieldAccess {
                            base: AmirOperand::Copy(base_tmp),
                            field: field_idx,
                        },
                    );
                    self.emit_goto(bb_join);
                }

                self.seal_block(bb_join);
                self.builder.current_block = Some(bb_join);
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::SafeIndex { base, index } => {
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                let base_op = self.lower_expr(*base, None, symbols)?;
                if self.builder.current_block.is_none() {
                    return Ok(AmirOperand::Copy(dest));
                }

                let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                self.emit_assign_temp(
                    cond_tmp,
                    AmirRvalue::Binary {
                        op: BinaryOp::Equal,
                        left: base_op,
                        right: AmirOperand::Constant(AmirConstant::Nil),
                    },
                );

                let bb_null = self.new_block();
                let bb_access = self.new_block();
                let bb_join = self.new_block();

                self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_null, bb_access);
                self.seal_block(bb_null);
                self.seal_block(bb_access);

                self.builder.current_block = Some(bb_null);
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil)),
                );
                self.emit_goto(bb_join);

                self.builder.current_block = Some(bb_access);
                let index_op = self.lower_expr(*index, None, symbols)?;
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::IndexAccess {
                        base: base_op,
                        index: index_op,
                    },
                );
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                self.seal_block(bb_join);
                self.builder.current_block = Some(bb_join);
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::NullCoalesce { left, right } => {
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                let left_expr = self.hir.pool.expr(*left);
                let left_is_option = self.with_ty(left_expr.ty, is_option_type);
                let left_op = self.lower_expr(*left, None, symbols)?;
                if self.builder.current_block.is_none() {
                    return Ok(AmirOperand::Copy(dest));
                }

                let bb_left = self.new_block();
                let bb_right = self.new_block();
                let bb_join = self.new_block();

                if left_is_option {
                    // Option: tag 1 is Some, tag 0 is None.
                    let tag_tmp = self.new_temp(ArType::Primitive(Primitive::Int));
                    self.emit_assign_temp(tag_tmp, AmirRvalue::Discriminant { value: left_op });
                    let one_lit = self.intern_literal_int("1");
                    let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        cond_tmp,
                        AmirRvalue::Binary {
                            op: BinaryOp::Equal,
                            left: AmirOperand::Copy(tag_tmp),
                            right: AmirOperand::Constant(one_lit),
                        },
                    );
                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_left, bb_right);
                    self.seal_block(bb_left);
                    self.seal_block(bb_right);

                    self.builder.current_block = Some(bb_left);
                    self.lower_result_ok_field(left_op, dest);
                    self.emit_goto(bb_join);
                } else {
                    let cond_tmp = self.new_temp(ArType::Primitive(Primitive::Bool));
                    self.emit_assign_temp(
                        cond_tmp,
                        AmirRvalue::Binary {
                            op: BinaryOp::NotEqual,
                            left: left_op,
                            right: AmirOperand::Constant(AmirConstant::Nil),
                        },
                    );
                    self.set_bool_branch(AmirOperand::Copy(cond_tmp), bb_left, bb_right);
                    self.seal_block(bb_left);
                    self.seal_block(bb_right);

                    self.builder.current_block = Some(bb_left);
                    self.emit_assign_temp(dest, AmirRvalue::Use(left_op));
                    self.emit_goto(bb_join);
                }

                self.builder.current_block = Some(bb_right);
                self.lower_expr(*right, Some(dest), symbols)?;
                if self.builder.current_block.is_some() {
                    self.emit_goto(bb_join);
                }

                self.seal_block(bb_join);
                self.builder.current_block = Some(bb_join);
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::Catch {
                expr: inner,
                handler,
            } => self.lower_catch(*inner, handler, target, expr.ty, symbols),
            HirExprKind::Lambda { .. } => Err(amir_unsupported(
                expr.span,
                "lambda/closure",
                "v0.3 LAMBDA: closure lowering",
            )),
            // A3.0/A3.1/A3.3: evaluate block as coroutine body (Suspend on nested await),
            // wrap payload as Coroutine[T]. Stack-first when not the function return slot.
            HirExprKind::AsyncBlock { block } => {
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr.ty));
                let payload_ty = match self.resolve_ty(expr.ty) {
                    ArType::Coroutine(inner) => inner,
                    _ => expr.ty,
                };
                let payload_tmp = self.new_temp_id(payload_ty);
                self.coroutine_depth = self.coroutine_depth.saturating_add(1);
                let lower_res = self.lower_block_as_expr(*block, Some(payload_tmp), symbols);
                self.coroutine_depth = self.coroutine_depth.saturating_sub(1);
                lower_res?;
                // A3.3: stack state unless this is the return register of a coroutine-
                // returning function (must outlive the callee).
                let stack = dest != TempId(0);
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::CoroutineReady {
                        value: AmirOperand::Copy(payload_tmp),
                        payload_ty,
                        stack,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
            HirExprKind::UnsafeBlock { .. } => Err(amir_unsupported(
                expr.span,
                "unsafe block expression",
                "v0.2 UNSAFE: unsafe legality and lowering",
            )),
            HirExprKind::Error => {
                let dest = self.new_temp(ArType::Error);
                Ok(AmirOperand::Copy(dest))
            }
        }
    }

    /// Fold `mem.sizeOf<T>()` / `mem.alignOf<T>()` (and bare `sizeOf`/`alignOf`)
    /// to integer constants using [`LayoutEngine`] (target pointer width).
    fn try_lower_mem_size_align_intrinsic(
        &mut self,
        callee: HirExprId,
        type_args: &[crate::types::TypeId],
        result_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<Option<AmirOperand>, Diagnostic> {
        if type_args.len() != 1 {
            return Ok(None);
        }
        let callee_expr = self.hir.pool.expr(callee);
        let name = match &callee_expr.kind {
            HirExprKind::Path { symbol } => symbols.get(*symbol).name.as_str(),
            HirExprKind::Field { field, .. } => field.as_str(),
            HirExprKind::TypePath { member_symbol, .. } => {
                symbols.get(*member_symbol).name.as_str()
            }
            _ => return Ok(None),
        };
        let kind = arandu_middle::IntrinsicKind::from_name(name);
        let bare = match kind {
            Some(arandu_middle::IntrinsicKind::SizeOf) => "sizeOf",
            Some(arandu_middle::IntrinsicKind::AlignOf) => "alignOf",
            _ => return Ok(None),
        };

        let ty = self.resolve_ty(type_args[0]);
        let engine = arandu_middle::layout::LayoutEngine::new(self.pointer_width);
        let layout = engine
            .layout_of_type(
                &ty,
                &self.tc.type_info.type_interner,
                self.tc.type_info.as_ref(),
            )
            .map_err(|error| {
                Diagnostic::ice(
                    DiagCode::ICEGEN002,
                    format!("failed to compute layout for mem.{bare}: {error}"),
                    callee_expr.span,
                )
            })?;
        let value = if kind == Some(arandu_middle::IntrinsicKind::SizeOf) {
            layout.size
        } else {
            layout.align
        };

        let lit = self.intern_literal_int(value.to_string());
        let dest = target.unwrap_or_else(|| self.new_temp_id(result_ty));
        self.emit_assign_temp(dest, AmirRvalue::Use(AmirOperand::Constant(lit)));
        Ok(Some(AmirOperand::Copy(dest)))
    }
}
