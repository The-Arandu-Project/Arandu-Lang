//! Pretty printing context and type formatting utilities.

use crate::SymbolTable;

pub struct HirPrettyCtx<'a> {
    pub pool: &'a crate::hir::HirPool,
    pub symbols: &'a SymbolTable,
    pub show_spans: bool,
    pub type_interner: Option<&'a crate::types::TypeInterner>,
}

impl HirPrettyCtx<'_> {
    /// Display a `TypeId` using the interner if available.
    pub fn display_ty(&self, ty: crate::types::TypeId) -> String {
        static EMPTY: std::sync::LazyLock<crate::types::TypeInterner> =
            std::sync::LazyLock::new(crate::types::TypeInterner::new);
        let interner = self.type_interner.unwrap_or(&EMPTY);
        interner.display(ty, self.symbols)
    }
}

pub(super) fn display_type(ty: crate::types::TypeId, ctx: &HirPrettyCtx<'_>) -> String {
    ctx.display_ty(ty)
}
