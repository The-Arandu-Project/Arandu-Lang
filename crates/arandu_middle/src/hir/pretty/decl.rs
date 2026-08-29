//! Declaration and top-level program pretty printing implementation.

use super::types::HirPrettyCtx;
use crate::hir::{
    HirConst, HirDecl, HirEnum, HirExtern, HirFunc, HirInterface, HirParam, HirProgram, HirStruct,
    HirTypeAlias, ReceiverKind, symbol_name,
};

pub(super) fn format_hir_param(p: &HirParam, ctx: &HirPrettyCtx<'_>) -> String {
    let name = symbol_name(ctx.symbols, p.symbol);
    let ty = ctx.display_ty(p.ty);
    if p.is_receiver {
        let prefix = match p.receiver_kind {
            Some(ReceiverKind::Shared) => "shared ",
            Some(ReceiverKind::Mut) => "mut ",
            Some(ReceiverKind::Own) => "own ",
            None => "",
        };
        format!("{prefix}{name}: {ty}")
    } else {
        format!("{name}: {ty}")
    }
}

pub fn print_program(program: &HirProgram, ctx: &HirPrettyCtx<'_>) -> String {
    let mut out = String::new();
    out.push_str("Program\n");
    if let Some(ref m) = program.module {
        out.push_str(&format!("  Module {m}\n"));
    }

    let mut first = true;
    for decl in &program.decls {
        if !first {
            out.push('\n');
        }
        first = false;
        decl.pretty_print_to(&mut out, 1, ctx);
    }
    out
}

impl HirDecl {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        match self {
            HirDecl::Const(c) => c.pretty_print_to(out, indent, ctx),
            HirDecl::TypeAlias(t) => t.pretty_print_to(out, indent, ctx),
            HirDecl::Func(f) => f.pretty_print_to(out, indent, ctx),
            HirDecl::Struct(s) => s.pretty_print_to(out, indent, ctx),
            HirDecl::Enum(e) => e.pretty_print_to(out, indent, ctx),
            HirDecl::Interface(i) => i.pretty_print_to(out, indent, ctx),
            HirDecl::Extern(ex) => ex.pretty_print_to(out, indent, ctx),
        }
    }
}

impl HirConst {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!(
            "{}Const {}: {} =\n",
            ind,
            symbol_name(ctx.symbols, self.symbol),
            ctx.display_ty(self.ty)
        ));
        self.value.pretty_print_to(out, indent + 1, ctx);
    }
}

impl HirTypeAlias {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!(
            "{}TypeAlias {} = {}\n",
            ind,
            symbol_name(ctx.symbols, self.symbol),
            ctx.display_ty(self.target)
        ));
    }
}

impl HirFunc {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        let params_str: Vec<String> = ctx
            .pool
            .params_list(self.params)
            .iter()
            .map(|p| format_hir_param(p, ctx))
            .collect();
        let return_ty_str = ctx.display_ty(self.return_type);
        out.push_str(&format!(
            "{}Func {}({}) -> {}\n",
            ind,
            symbol_name(ctx.symbols, self.symbol),
            params_str.join(", "),
            return_ty_str
        ));
        if let Some(body_id) = self.body {
            ctx.pool
                .block(body_id)
                .pretty_print_to(out, indent + 1, ctx);
        }
    }
}

impl HirStruct {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!(
            "{}Struct {}\n",
            ind,
            symbol_name(ctx.symbols, self.symbol)
        ));
        let field_ind = "  ".repeat(indent + 1);
        for f in ctx.pool.struct_fields_list(self.fields) {
            out.push_str(&format!(
                "{}{}: {}\n",
                field_ind,
                symbol_name(ctx.symbols, f.symbol),
                ctx.display_ty(f.ty)
            ));
        }
    }
}

impl HirEnum {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!(
            "{}Enum {}\n",
            ind,
            symbol_name(ctx.symbols, self.symbol)
        ));
        let variant_ind = "  ".repeat(indent + 1);
        for v in ctx.pool.enum_variants_list(self.variants) {
            if let Some(ref payload) = v.payload {
                out.push_str(&format!(
                    "{}{}({})\n",
                    variant_ind,
                    symbol_name(ctx.symbols, v.symbol),
                    ctx.display_ty(*payload)
                ));
            } else {
                out.push_str(&format!(
                    "{}{}\n",
                    variant_ind,
                    symbol_name(ctx.symbols, v.symbol)
                ));
            }
        }
    }
}

impl HirInterface {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!(
            "{}Interface {}\n",
            ind,
            symbol_name(ctx.symbols, self.symbol)
        ));
    }
}

impl HirExtern {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        out.push_str(&format!("{}Extern \"{}\"\n", ind, self.abi));
        let member_ind = "  ".repeat(indent + 1);
        for m in ctx.pool.func_signatures_list(self.members) {
            let params_str: Vec<String> = ctx
                .pool
                .params_list(m.params)
                .iter()
                .map(|p| format_hir_param(p, ctx))
                .collect();
            out.push_str(&format!(
                "{}Func {}({}) -> {}\n",
                member_ind,
                symbol_name(ctx.symbols, m.symbol),
                params_str.join(", "),
                ctx.display_ty(m.return_type)
            ));
        }
    }
}
