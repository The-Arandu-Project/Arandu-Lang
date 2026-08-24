use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{
    HirConst, HirDecl, HirEnum, HirEnumVariant, HirExtern, HirFunc, HirFuncSignature, HirInterface,
    HirParam, HirStruct, HirStructField, HirTypeAlias,
};
use crate::passes::lowering::require_def_symbol;
use crate::passes::type_checker::types::ArType;
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_parser::ast_pool::AstPool;
use arandu_parser::{FuncName, TopLevelDecl};

fn error_ty() -> TypeId {
    TypeInterner::preinterned_error_id()
}

pub(crate) fn lower_decl(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    decl: &TopLevelDecl,
) -> Result<Option<HirDecl>, Diagnostic> {
    match decl {
        TopLevelDecl::Const(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let ty = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            let value_vid = super::expr::lower_expr(type_check, pool, hir_pool, d.value)?;
            Ok(Some(HirDecl::Const(HirConst {
                symbol,
                ty,
                value: value_vid,
                span: d.span,
            })))
        }
        TopLevelDecl::TypeAlias(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let target = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            Ok(Some(HirDecl::TypeAlias(HirTypeAlias {
                symbol,
                target,
                span: d.span,
            })))
        }
        TopLevelDecl::Func(d) => {
            let name_span = match &d.name {
                arandu_parser::FuncName::Free { span, .. } => *span,
                arandu_parser::FuncName::Method { span, .. } => *span,
            };
            let symbol = require_def_symbol(&type_check.resolved, name_span)?;
            let decl_ty_id = type_check
                .type_info
                .decl_type_id(symbol)
                .unwrap_or_else(error_ty);
            let return_type = match type_check.type_info.type_interner.resolve(decl_ty_id) {
                ArType::Func(_, ret) => ret,
                _ => decl_ty_id,
            };
            let mut params = Vec::new();
            for p in &d.params {
                let p_symbol = require_def_symbol(&type_check.resolved, p.span)?;
                let p_ty = type_check
                    .type_info
                    .decl_type_id(p_symbol)
                    .unwrap_or_else(error_ty);
                params.push(HirParam {
                    symbol: p_symbol,
                    ty: p_ty,
                    span: p.span,
                    is_receiver: p.is_receiver,
                    receiver_kind: p.ownership.map(super::stmt::ownership_to_receiver_kind),
                });
            }
            let params = hir_pool.alloc_param_list(&params);
            let annotation_target = if matches!(d.name, FuncName::Method { .. }) {
                crate::attributes::AnnotationTarget::Method
            } else {
                crate::attributes::AnnotationTarget::Function
            };
            let no_fallback =
                crate::attributes::validate_attributes(&d.attrs, annotation_target, pool)
                    .contains(crate::attributes::AnnotationId::NoFallback);
            Ok(Some(HirDecl::Func(HirFunc {
                symbol,
                params,
                return_type,
                body: Some(super::stmt::lower_block(
                    type_check, pool, hir_pool, &d.body,
                )?),
                span: d.span,
                is_async: d.is_async,
                no_fallback,
            })))
        }
        TopLevelDecl::Struct(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let mut fields = Vec::new();
            if let Some(struct_fields_map) = type_check.type_info.struct_fields.get(&symbol) {
                for f in &d.fields {
                    let field_symbol = require_def_symbol(&type_check.resolved, f.span)?;
                    let field_ty = struct_fields_map
                        .get(f.name.as_str())
                        .copied()
                        .unwrap_or_else(error_ty);
                    fields.push(HirStructField {
                        symbol: field_symbol,
                        ty: field_ty,
                        span: f.span,
                    });
                }
            }
            let fields = hir_pool.alloc_struct_field_list(&fields);
            Ok(Some(HirDecl::Struct(HirStruct {
                symbol,
                fields,
                span: d.span,
            })))
        }
        TopLevelDecl::Enum(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            let mut variants = Vec::new();
            for v in &d.variants {
                let v_symbol = require_def_symbol(&type_check.resolved, v.span)?;
                let payload =
                    type_check
                        .type_info
                        .enum_variants
                        .get(&v_symbol)
                        .and_then(|(_, shape)| match shape {
                            crate::passes::type_checker::EnumPayloadShape::Unit => None,
                            crate::passes::type_checker::EnumPayloadShape::Tuple(tids) => {
                                if tids.is_empty() {
                                    None
                                } else if tids.len() == 1 {
                                    Some(tids[0])
                                } else {
                                    let interner = &type_check.type_info.type_interner;
                                    Some(interner.intern(ArType::Tuple(tids.clone())))
                                }
                            }
                        });
                variants.push(HirEnumVariant {
                    symbol: v_symbol,
                    payload,
                    span: v.span,
                });
            }
            let variants = hir_pool.alloc_enum_variant_list(&variants);
            Ok(Some(HirDecl::Enum(HirEnum {
                symbol,
                variants,
                span: d.span,
            })))
        }
        TopLevelDecl::Interface(d) => {
            let symbol = require_def_symbol(&type_check.resolved, d.span)?;
            Ok(Some(HirDecl::Interface(HirInterface {
                symbol,
                span: d.span,
            })))
        }
        TopLevelDecl::Extern(d) => {
            let mut members = Vec::new();
            for m in &d.members {
                let symbol = require_def_symbol(&type_check.resolved, m.span)?;
                let m_ty_id = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or_else(error_ty);
                let return_type = match type_check.type_info.type_interner.resolve(m_ty_id) {
                    ArType::Func(_, ret) => ret,
                    _ => m_ty_id,
                };
                let mut params = Vec::new();
                for p in &m.params {
                    let p_symbol = require_def_symbol(&type_check.resolved, p.span)?;
                    let p_ty = type_check
                        .type_info
                        .decl_type_id(p_symbol)
                        .unwrap_or_else(error_ty);
                    params.push(HirParam {
                        symbol: p_symbol,
                        ty: p_ty,
                        span: p.span,
                        is_receiver: p.is_receiver,
                        receiver_kind: p.ownership.map(super::stmt::ownership_to_receiver_kind),
                    });
                }
                let params = hir_pool.alloc_param_list(&params);
                members.push(HirFuncSignature {
                    symbol,
                    params,
                    return_type,
                    span: m.span,
                });
            }
            let members = hir_pool.alloc_func_signature_list(&members);
            Ok(Some(HirDecl::Extern(HirExtern {
                abi: d.abi.to_string(),
                members,
                span: d.span,
            })))
        }
        TopLevelDecl::Error(_) => Ok(None),
    }
}
