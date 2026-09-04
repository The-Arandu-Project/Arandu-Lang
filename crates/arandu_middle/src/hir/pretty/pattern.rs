//! Pattern and field pattern formatting implementation.

use super::types::HirPrettyCtx;
use crate::hir::HirPattern;

pub(super) fn format_pattern_ref(pat: &HirPattern, ctx: &HirPrettyCtx<'_>) -> String {
    match pat {
        HirPattern::Wildcard { span } => {
            format!("Wildcard {{ span: {:?} }}", span)
        }
        HirPattern::Bind { span, name, symbol } => {
            format!(
                "Bind {{ span: {:?}, name: {:?}, symbol: {:?} }}",
                span, name, symbol
            )
        }
        HirPattern::Literal { span, expr } => {
            format!("Literal {{ span: {:?}, expr: {:?} }}", span, expr)
        }
        HirPattern::Enum {
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
        } => {
            let mut payload_strs = Vec::new();
            for &pid in ctx.pool.pattern_list(*payload) {
                payload_strs.push(format_pattern_ref(ctx.pool.pattern(pid), ctx));
            }
            format!(
                "Enum {{ span: {:?}, type_symbol: {:?}, variant: {:?}, variant_symbol: {:?}, payload: [{}] }}",
                span,
                type_symbol,
                variant,
                variant_symbol,
                payload_strs.join(", ")
            )
        }
        HirPattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            let mut payload_strs = Vec::new();
            for &pid in ctx.pool.pattern_list(*payload) {
                payload_strs.push(format_pattern_ref(ctx.pool.pattern(pid), ctx));
            }
            format!(
                "TypeTuple {{ span: {:?}, name: {:?}, payload: [{}] }}",
                span,
                name,
                payload_strs.join(", ")
            )
        }
        HirPattern::Struct {
            span,
            struct_symbol,
            fields,
        } => {
            let mut field_strs = Vec::new();
            for &fid in ctx.pool.field_pattern_list(*fields) {
                let f = ctx.pool.field_pattern(fid);
                let pat_str = f.pattern.map_or("None".to_string(), |pid| {
                    format!("Some({})", format_pattern_ref(ctx.pool.pattern(pid), ctx))
                });
                field_strs.push(format!(
                    "HirFieldPattern {{ span: {:?}, name: {:?}, pattern: {} }}",
                    f.span, f.name, pat_str
                ));
            }
            format!(
                "Struct {{ span: {:?}, struct_symbol: {:?}, fields: [{}] }}",
                span,
                struct_symbol,
                field_strs.join(", ")
            )
        }
        HirPattern::Tuple { span, items } => {
            let mut item_strs = Vec::new();
            for &pid in ctx.pool.pattern_list(*items) {
                item_strs.push(format_pattern_ref(ctx.pool.pattern(pid), ctx));
            }
            format!(
                "Tuple {{ span: {:?}, items: [{}] }}",
                span,
                item_strs.join(", ")
            )
        }
        HirPattern::Range {
            span,
            start,
            inclusive,
            end,
        } => {
            format!(
                "Range {{ span: {:?}, start: {:?}, inclusive: {:?}, end: {:?} }}",
                span, start, inclusive, end
            )
        }
        HirPattern::Or { span, alts } => {
            let mut alt_strs = Vec::new();
            for &pid in ctx.pool.pattern_list(*alts) {
                alt_strs.push(format_pattern_ref(ctx.pool.pattern(pid), ctx));
            }
            format!(
                "Or {{ span: {:?}, alts: [{}] }}",
                span,
                alt_strs.join(" | ")
            )
        }
    }
}

impl HirPattern {
    pub(super) fn pretty_print_to(&self, out: &mut String, _indent: usize, ctx: &HirPrettyCtx<'_>) {
        out.push_str(&format_pattern_ref(self, ctx));
    }
}
