use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_parser::{BinaryOp, UnaryOp};

use super::synth_expr;
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::types::{self, ArType, Primitive};
use arandu_middle::types::type_interner::TypeId;

pub(super) fn cast_types_compatible(
    found: &ArType,
    target: &ArType,
    interner: &arandu_middle::types::TypeInterner,
    symbols: &arandu_middle::SymbolTable,
) -> bool {
    if found.is_error() || target.is_error() {
        return true;
    }
    if types::unify(found, target, interner) {
        return true;
    }
    if found.is_numeric() && target.is_numeric() {
        return true;
    }
    // `char` is a Unicode scalar value with a canonical u32 representation.
    // This direction is lossless; the inverse requires scalar validation and
    // therefore remains outside the general cast operator.
    if matches!(found, ArType::Primitive(Primitive::Char))
        && matches!(target, ArType::Primitive(Primitive::U32))
    {
        return true;
    }
    if matches!(found, ArType::Ptr(_)) && matches!(target, ArType::Ptr(_)) {
        return true;
    }
    // A3/SL_R ABI: `Coroutine[T]` is a state-blob pointer at runtime (Cranelift
    // `clif_type` → pointer). Allow `job as ptr[u8]` so `std.runtime` typed
    // spawn/block_on can hand the blob to host `ar_rt_*` / `ar_co_*`.
    if let (ArType::Coroutine(_), ArType::Ptr(inner)) = (found, target) {
        let inner_ty = interner.resolve(*inner);
        return matches!(
            inner_ty,
            ArType::Primitive(Primitive::Byte) | ArType::Primitive(Primitive::U8)
        );
    }
    // Host i64 payload bits → type param `T` (generic join/block_on).
    // Only TypeParam targets — never arbitrary named types.
    if found.is_integer()
        && let ArType::Named(id, args) = target
        && args.is_empty()
        && symbols.get(*id).kind == arandu_middle::SymbolKind::TypeParam
    {
        return true;
    }
    false
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, _expr))]
pub(super) fn synth_binary_unary_expr(
    checker: &mut TypeChecker<'_>,
    _expr: ExprId,
    kind: &ExprKind,
    span: Span,
) -> Option<TypeId> {
    match kind {
        ExprKind::NullCoalesce { left, right } => {
            let left_id = *left;
            let right_id = *right;
            let left_ty_id = synth_expr(checker, left_id);
            let right_ty_id = synth_expr(checker, right_id);
            let interner = &checker.type_info.type_interner;
            let left_ty = interner.resolve(left_ty_id);
            match left_ty {
                ArType::Nullable(inner) => {
                    let inner_id = inner;
                    if !checker.unify_ids(inner_id, right_ty_id) {
                        checker.add_constraint(
                            inner_id,
                            right_ty_id,
                            ConstraintOrigin::NullCoalesce {
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(right_ty_id)
                }
                ArType::Error => Some(right_ty_id),
                other => {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T006NotNullable,
                            format!(
                                "operator `??` requires a nullable left-hand side, found '{}'",
                                other.display(&checker.symbols, interner)
                            ),
                            span,
                        )
                        .with_label(
                            checker.pool.expr_span(left_id),
                            format!("type is '{}'", other.display(&checker.symbols, interner)),
                        )
                        .with_hint(
                            "use a nullable value on the left or make it nullable".to_string(),
                        ),
                    );
                    Some(right_ty_id)
                }
            }
        }
        ExprKind::Cast {
            expr: inner_expr,
            ty,
        } => {
            let inner_id = *inner_expr;
            let ty_id = *ty;
            let found_ty_id = synth_expr(checker, inner_id);
            let target_ty = checker.lower_type_expr(ty_id, checker.type_scope());
            let target_ty_id = checker.intern(target_ty);
            let found_ty = checker.resolve(found_ty_id);
            let target_ty = checker.resolve(target_ty_id);
            if !cast_types_compatible(
                &found_ty,
                &target_ty,
                &checker.type_info.type_interner,
                &checker.symbols,
            ) {
                checker.add_constraint(
                    target_ty_id,
                    found_ty_id,
                    ConstraintOrigin::CastExpr {
                        expr_span: checker.pool.expr_span(inner_id),
                        target_span: checker.pool.type_expr_span(ty_id),
                    },
                );
            }
            Some(target_ty_id)
        }
        ExprKind::Unary {
            op,
            expr: inner_expr,
        } => {
            let inner_id = *inner_expr;
            let expr_ty_id = synth_expr(checker, inner_id);
            let interner = &checker.type_info.type_interner;
            let expr_ty = interner.resolve(expr_ty_id);
            if expr_ty.is_error() {
                return Some(checker.intern(ArType::Error));
            }
            match op {
                UnaryOp::Neg => {
                    if expr_ty.is_numeric() {
                        Some(expr_ty_id)
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Int),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::Not => {
                    if types::unify(&expr_ty, &ArType::Primitive(Primitive::Bool), interner) {
                        Some(checker.intern(ArType::Primitive(Primitive::Bool)))
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Bool),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::BitNot => {
                    if expr_ty.is_integer() {
                        Some(expr_ty_id)
                    } else {
                        checker.add_constraint(
                            ArType::Primitive(Primitive::Int),
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                UnaryOp::Await => {
                    if expr_ty.is_error() {
                        Some(checker.intern(ArType::Error))
                    } else if let ArType::Coroutine(inner) = expr_ty {
                        Some(inner)
                    } else {
                        checker.add_constraint(
                            ArType::Error,
                            expr_ty_id,
                            ConstraintOrigin::AwaitInvalid { span },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                }
                // F2.0: safe shared/exclusive address-of (not raw ptr).
                UnaryOp::Ref => {
                    let inner_id = if expr_ty.is_error() {
                        checker.intern(ArType::Error)
                    } else {
                        expr_ty_id
                    };
                    Some(checker.intern(ArType::Ref(inner_id)))
                }
                UnaryOp::RefMut => {
                    let inner_id = if expr_ty.is_error() {
                        checker.intern(ArType::Error)
                    } else {
                        expr_ty_id
                    };
                    Some(checker.intern(ArType::RefMut(inner_id)))
                }
                UnaryOp::Deref => match expr_ty {
                    ArType::Ref(inner) | ArType::RefMut(inner) => Some(inner),
                    ArType::Ptr(inner) => {
                        if !checker.ctx.is_in_unsafe() {
                            checker.diagnostics.push(
                                crate::Diagnostic::error(
                                    crate::DiagCode::O012AllocRequiresUnsafe,
                                    "dereferencing a raw pointer requires an `unsafe` block",
                                    span,
                                )
                                .with_label(span, "raw `ptr[T]` deref is unsafe")
                                .with_note(
                                    "use `&T` / `&mut T` for safe borrows (F2.0)".to_string(),
                                ),
                            );
                        }
                        Some(inner)
                    }
                    _ => {
                        checker.add_constraint(
                            ArType::Error,
                            expr_ty_id,
                            ConstraintOrigin::UnaryOp {
                                op_span: span,
                                operand_span: checker.pool.expr_span(inner_id),
                            },
                        );
                        Some(checker.intern(ArType::Error))
                    }
                },
            }
        }
        ExprKind::Binary { op, left, right } => {
            let left_id = *left;
            let right_id = *right;
            let mut left_ty_id = synth_expr(checker, left_id);
            let left_ty = checker.resolve(left_ty_id);
            let right_expected = if !left_ty.is_literal() && left_ty.is_numeric() {
                Some(left_ty_id)
            } else {
                None
            };
            let right_ty_id = super::super::synth_expr_expected(checker, right_id, right_expected);
            let right_ty = checker.resolve(right_ty_id);
            if left_ty.is_literal() && !right_ty.is_literal() && right_ty.is_numeric() {
                left_ty_id = super::super::synth_expr_expected(checker, left_id, Some(right_ty_id));
            }
            let interner = &checker.type_info.type_interner;
            let left_ty = interner.resolve(left_ty_id);
            let right_ty = interner.resolve(right_ty_id);

            if left_ty.is_error() || right_ty.is_error() {
                return Some(checker.intern(ArType::Error));
            }

            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || (!left_ty.is_numeric() && !right_ty.is_numeric())
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    Some(checker.intern(types::resolve_literal_pair(&left_ty, &right_ty)))
                }
                BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::BitAnd
                | BinaryOp::ShiftLeft
                | BinaryOp::ShiftRight => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || !left_ty.is_integer()
                        || !right_ty.is_integer()
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    Some(checker.intern(types::resolve_literal_pair(&left_ty, &right_ty)))
                }
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Lt
                | BinaryOp::Gt
                | BinaryOp::LtEqual
                | BinaryOp::GtEqual => {
                    // Root cause fix (RC-ERR-NIL): `x != nil` / `x == nil` where `x` is
                    // `T?` / `Option<T>` / Result-destructure err channel. Bare `nil`
                    // otherwise defaults to `void?` and fails unify with `Err`.
                    let left_is_nil = matches!(checker.pool.expr(left_id), ExprKind::Nil);
                    let right_is_nil = matches!(checker.pool.expr(right_id), ExprKind::Nil);
                    if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
                        && (left_is_nil || right_is_nil)
                    {
                        let (value_ty_id, nil_expr) = if right_is_nil {
                            (left_ty_id, right_id)
                        } else {
                            (right_ty_id, left_id)
                        };
                        let value_ty = checker.resolve(value_ty_id);
                        let ok = matches!(value_ty, ArType::Nullable(_) | ArType::Option(_))
                            || value_ty.is_error();
                        if ok {
                            // Re-record nil with the value's type so HIR is consistent.
                            checker.record_expr_type(nil_expr, value_ty_id);
                            return Some(checker.intern(ArType::Primitive(Primitive::Bool)));
                        }
                    }

                    if !checker.unify_ids(left_ty_id, right_ty_id) {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(checker.intern(ArType::Primitive(Primitive::Bool)))
                }
                BinaryOp::RangeExclusive | BinaryOp::RangeInclusive => {
                    let left_ty = checker.resolve(left_ty_id);
                    let right_ty = checker.resolve(right_ty_id);
                    if !checker.unify_ids(left_ty_id, right_ty_id)
                        || (!left_ty.is_integer() && !right_ty.is_integer())
                    {
                        checker.add_constraint(
                            left_ty_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                        return Some(checker.intern(ArType::Error));
                    }
                    let inner_ty =
                        types::resolve_literal_pair(&left_ty, &right_ty).default_literal();
                    let inner_id = checker.intern(inner_ty);
                    Some(checker.intern(ArType::Range(inner_id)))
                }
                BinaryOp::And | BinaryOp::Or => {
                    let bool_id = checker.intern(ArType::Primitive(Primitive::Bool));
                    if !checker.unify_ids(left_ty_id, bool_id) {
                        checker.add_constraint(
                            bool_id,
                            left_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    if !checker.unify_ids(right_ty_id, bool_id) {
                        checker.add_constraint(
                            bool_id,
                            right_ty_id,
                            ConstraintOrigin::BinaryOp {
                                op_span: span,
                                left_span: checker.pool.expr_span(left_id),
                                right_span: checker.pool.expr_span(right_id),
                            },
                        );
                    }
                    Some(bool_id)
                }
                _ => Some(checker.intern(ArType::Error)),
            }
        }
        _ => None,
    }
}
