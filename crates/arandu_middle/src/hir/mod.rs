mod pattern;
mod pretty;
pub use pattern::{HirFieldPattern, HirPattern};
pub use pretty::HirPrettyCtx;

use crate::ops::{BinaryOp, SetOp, UnaryOp};
use crate::types::TypeId;
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;
use smallvec::SmallVec;
use smol_str::SmolStr;

pub use pool::{
    HirBlockId, HirDeclId, HirEnumVariantId, HirExprId, HirFieldPatternId, HirFuncSignatureId,
    HirParamId, HirPatternId, HirPool, HirStmtId, HirStructFieldId, IndexRange,
};

#[must_use]
pub fn symbol_name(symbols: &SymbolTable, id: SymbolId) -> &str {
    &symbols.get(id).name
}

#[derive(Debug)]
pub struct HirProgram {
    pub span: Span,
    pub module: Option<String>,
    pub decls: Vec<HirDeclId>,
    pub pool: crate::hir::HirPool,
}

#[derive(Debug, Clone)]
pub enum HirDecl {
    Const(HirConst),
    TypeAlias(HirTypeAlias),
    Func(HirFunc),
    Struct(HirStruct),
    Enum(HirEnum),
    Interface(HirInterface),
    Extern(HirExtern),
}

impl HirDecl {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            HirDecl::Const(decl) => decl.span,
            HirDecl::TypeAlias(decl) => decl.span,
            HirDecl::Func(decl) => decl.span,
            HirDecl::Struct(decl) => decl.span,
            HirDecl::Enum(decl) => decl.span,
            HirDecl::Interface(decl) => decl.span,
            HirDecl::Extern(decl) => decl.span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub value: HirExprId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirTypeAlias {
    pub symbol: SymbolId,
    pub target: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirFunc {
    pub symbol: SymbolId,
    pub params: IndexRange,
    pub return_type: TypeId,
    pub body: Option<HirBlockId>,
    pub span: Span,
    /// A3: `async func` — return type is `Coroutine[T]`; body returns bare `T` and
    /// AMIR wraps with `CoroutineReady` (type sugar, not a separate colour world).
    pub is_async: bool,
    /// G2 / F2.3.3: `@NoFallback` — promote generational-escape O004 notes to errors.
    /// Not a silent strict mode: only affects scopes that opt in.
    pub no_fallback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiverKind {
    Shared,
    Mut,
    Own,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
    pub is_receiver: bool,
    pub receiver_kind: Option<ReceiverKind>,
}

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub symbol: SymbolId,
    pub fields: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStructField {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub symbol: SymbolId,
    pub variants: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub symbol: SymbolId,
    pub payload: Option<TypeId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirInterface {
    pub symbol: SymbolId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirExtern {
    pub abi: String,
    pub members: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirFuncSignature {
    pub symbol: SymbolId,
    pub params: IndexRange,
    pub return_type: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub statements: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub kind: HirStmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    VarDecl {
        bindings: IndexRange,
        value: HirExprId,
    },
    Set {
        places: IndexRange,
        op: SetOp,
        value: HirExprId,
    },
    Return {
        values: IndexRange,
    },
    Break,
    Continue,
    Free(HirExprId),
    Expr(HirExprId),
    If {
        condition: HirCondition,
        then_block: HirBlockId,
        else_block: Option<HirBlockId>,
    },
    For {
        clause: HirForClause,
        body: HirBlockId,
    },
    While {
        condition: HirCondition,
        body: HirBlockId,
    },
    Match {
        value: HirExprId,
        arms: IndexRange,
    },
    Defer(HirBlockId),
    ErrDefer(HirBlockId),
    Unsafe(HirBlockId),
    Error,
}

#[derive(Debug, Clone)]
pub enum HirCondition {
    Expr(HirExprId),
    Is {
        expr: HirExprId,
        pattern: HirPatternId,
    },
}

#[derive(Debug, Clone)]
pub enum HirForClause {
    In {
        span: Span,
        bindings: IndexRange,
        iterable: HirExprId,
    },
    CStyle {
        span: Span,
        init: Option<HirSimpleStmt>,
        condition: Option<HirExprId>,
        step: Option<HirSimpleStmt>,
    },
}

#[derive(Debug, Clone)]
pub struct HirForBinding {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirSimpleStmt {
    VarDecl {
        bindings: IndexRange,
        value: HirExprId,
    },
    Set {
        places: IndexRange,
        op: SetOp,
        value: HirExprId,
    },
    Expr(HirExprId),
}

#[derive(Debug, Clone)]
pub struct HirBindingItem {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirPlace {
    pub root_symbol: SymbolId,
    pub suffixes: SmallVec<[HirPlaceSuffix; 2]>,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirPlaceSuffix {
    Field {
        span: Span,
        name: SmolStr,
        field_symbol: Option<SymbolId>,
        ty: TypeId,
    },
    Index {
        span: Span,
        expr: HirExprId,
        ty: TypeId,
    },
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: TypeId,
    pub span: Span,
}

/// Builtin `Result.Ok` / `Result.Err` / `Option.Some` constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCtorVariant {
    Ok,
    Err,
    Some,
    /// Unit `Option.None` (no payload).
    None,
    /// A3.6: `Poll.Ready(v)`
    PollReady,
    /// A3.6: `Poll.Pending`
    PollPending,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    Path {
        symbol: SymbolId,
    },
    TypePath {
        type_symbol: SymbolId,
        member_symbol: SymbolId,
    },
    Generic {
        callee: HirExprId,
        args: Vec<TypeId>,
    },
    Field {
        base: HirExprId,
        field: SmolStr,
    },
    SafeField {
        base: HirExprId,
        field: SmolStr,
    },
    Index {
        base: HirExprId,
        index: HirExprId,
    },
    SafeIndex {
        base: HirExprId,
        index: HirExprId,
    },
    Try {
        expr: HirExprId,
    },
    Call {
        callee: HirExprId,
        args: IndexRange,
        trailing_block: Option<HirBlockId>,
    },
    ResultCtor {
        variant: ResultCtorVariant,
        value: HirExprId,
    },
    StructLiteral {
        struct_symbol: SymbolId,
        fields: IndexRange,
    },
    Array {
        items: IndexRange,
    },
    Lambda {
        params: IndexRange,
        body: HirLambdaBody,
    },
    Alloc {
        expr: HirExprId,
    },
    AsyncBlock {
        block: HirBlockId,
    },
    UnsafeBlock {
        block: HirBlockId,
    },
    If {
        condition: HirCondition,
        then_block: HirBlockId,
        else_block: HirBlockId,
    },
    Match {
        value: HirExprId,
        arms: IndexRange,
    },
    Catch {
        expr: HirExprId,
        handler: HirCatchHandler,
    },
    NullCoalesce {
        left: HirExprId,
        right: HirExprId,
    },
    Cast {
        expr: HirExprId,
        target_ty: TypeId,
    },
    Unary {
        op: UnaryOp,
        expr: HirExprId,
    },
    Binary {
        op: BinaryOp,
        left: HirExprId,
        right: HirExprId,
    },
    Int(SmolStr),
    Float(SmolStr),
    Bool(bool),
    Char(SmolStr),
    Str(SmolStr),
    /// String interpolation: a sequence of literal text segments and sub-expressions
    /// that are concatenated at runtime to produce a `str` value.
    StringInterp {
        parts: Vec<HirStringPart>,
    },
    /// Compiler intrinsic: format a ToStr-v0.1 value as `str` (`x.to_str()`).
    ToStr {
        value: HirExprId,
    },
    Nil,
    Error,
}

/// One segment of an interpolated string.
#[derive(Debug, Clone)]
pub enum HirStringPart {
    /// A literal text segment (already known at compile time).
    Text(SmolStr),
    /// A sub-expression whose runtime value is converted to string and concatenated.
    Expr(HirExprId),
}

#[derive(Debug, Clone)]
pub struct HirFieldInit {
    pub span: Span,
    pub name: SmolStr,
    pub value: HirExprId,
}

#[derive(Debug, Clone)]
pub struct HirLambdaParam {
    pub span: Span,
    pub symbol: SymbolId,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
pub enum HirLambdaBody {
    Expr(HirExprId),
    Block(HirBlockId),
}

#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub span: Span,
    pub pattern: HirPatternId,
    pub guard: Option<HirExprId>,
    pub body: HirMatchArmBody,
}

#[derive(Debug, Clone)]
pub enum HirMatchArmBody {
    Expr(HirExprId),
    Block(HirBlockId),
}

#[derive(Debug, Clone)]
pub enum HirCatchHandler {
    Expr(HirExprId),
    Block {
        error_symbol: Option<SymbolId>,
        error_name: Option<String>,
        block: HirBlockId,
    },
}

impl HirProgram {
    #[must_use]
    pub fn pretty_print(&self, ctx: &HirPrettyCtx<'_>) -> String {
        pretty::print_program(self, ctx)
    }
}

fn check_span(span: Span) -> Result<(), String> {
    if span.start == 0 && span.end == 0 {
        return Ok(());
    }
    if span.start > span.end {
        return Err(format!(
            "Invalid span: start {} is greater than end {}",
            span.start, span.end
        ));
    }
    Ok(())
}

impl HirProgram {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        for decl_id in &self.decls {
            let decl = pool.decl(*decl_id);
            check_span(decl.span())?;
            match decl {
                HirDecl::Const(c) => {
                    let _sym = symbols.get(c.symbol);
                    pool.expr(c.value).validate_invariants(pool, symbols)?;
                }
                HirDecl::TypeAlias(_) => {}
                HirDecl::Func(f) => {
                    check_span(f.span)?;
                    for param in pool.params_list(f.params) {
                        check_span(param.span)?;
                    }
                    if let Some(body_id) = f.body {
                        pool.block(body_id).validate_invariants(pool, symbols)?;
                    }
                }
                HirDecl::Struct(s) => {
                    check_span(s.span)?;
                    for field in pool.struct_fields_list(s.fields) {
                        check_span(field.span)?;
                    }
                }
                HirDecl::Enum(e) => {
                    check_span(e.span)?;
                    for var in pool.enum_variants_list(e.variants) {
                        check_span(var.span)?;
                    }
                }
                HirDecl::Interface(i) => {
                    check_span(i.span)?;
                }
                HirDecl::Extern(ex) => {
                    check_span(ex.span)?;
                    for m in pool.func_signatures_list(ex.members) {
                        check_span(m.span)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl HirBlock {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        let mut last_start = 0;
        for stmt_id in pool.stmt_list(self.statements) {
            let stmt = pool.stmt(*stmt_id);
            check_span(stmt.span)?;
            if stmt.span.start != 0 || stmt.span.end != 0 {
                if stmt.span.start < last_start {
                    return Err(format!(
                        "Block statement order out of sequence: span start {} is less than last start {}",
                        stmt.span.start, last_start
                    ));
                }
                last_start = stmt.span.start;
            }
            stmt.validate_invariants(pool, symbols)?;
        }
        Ok(())
    }
}

impl HirStmt {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        match &self.kind {
            HirStmtKind::VarDecl { bindings, value } => {
                for b in pool.bindings_list(*bindings) {
                    check_span(b.span)?;
                    if b.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Variable declaration binding '{}' has Error type",
                            symbol_name(symbols, b.symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Set {
                places,
                op: _,
                value,
            } => {
                let places_slice = pool.places_list(*places);
                if places_slice.is_empty() {
                    return Err("Set statement has no target places".to_string());
                }
                for p in places_slice {
                    check_span(p.span)?;
                    if p.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Set target place '{}' has Error type",
                            symbol_name(symbols, p.root_symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Return { values } => {
                for &v in pool.expr_list(*values) {
                    pool.expr(v).validate_invariants(pool, symbols)?;
                }
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
            HirStmtKind::Free(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Expr(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*then_block).validate_invariants(pool, symbols)?;
                if let Some(eb) = else_block {
                    pool.block(*eb).validate_invariants(pool, symbols)?;
                }
            }
            HirStmtKind::For { clause, body } => {
                match clause {
                    HirForClause::In {
                        bindings, iterable, ..
                    } => {
                        for b in pool.for_bindings_list(*bindings) {
                            check_span(b.span)?;
                        }
                        pool.expr(*iterable).validate_invariants(pool, symbols)?;
                    }
                    HirForClause::CStyle {
                        init,
                        condition,
                        step,
                        ..
                    } => {
                        if let Some(i) = init {
                            i.validate_invariants(pool, symbols)?;
                        }
                        if let Some(c) = condition {
                            pool.expr(*c).validate_invariants(pool, symbols)?;
                        }
                        if let Some(s) = step {
                            s.validate_invariants(pool, symbols)?;
                        }
                    }
                }
                pool.block(*body).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::While { condition, body } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*body).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Match { value, arms } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
                for arm in pool.match_arms_list(*arms) {
                    check_span(arm.span)?;
                    if let Some(g) = &arm.guard {
                        pool.expr(*g).validate_invariants(pool, symbols)?;
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            pool.expr(*e).validate_invariants(pool, symbols)?
                        }
                        HirMatchArmBody::Block(b) => {
                            pool.block(*b).validate_invariants(pool, symbols)?
                        }
                    }
                }
            }
            HirStmtKind::Defer(b) | HirStmtKind::ErrDefer(b) | HirStmtKind::Unsafe(b) => {
                pool.block(*b).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Error => {}
        }
        Ok(())
    }
}

impl HirCondition {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        match self {
            HirCondition::Expr(expr) => pool.expr(*expr).validate_invariants(pool, symbols),
            HirCondition::Is { expr, .. } => pool.expr(*expr).validate_invariants(pool, symbols),
        }
    }
}

impl HirSimpleStmt {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        match self {
            HirSimpleStmt::VarDecl { bindings, value } => {
                for b in pool.bindings_list(*bindings) {
                    check_span(b.span)?;
                    if b.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Variable declaration binding '{}' has Error type",
                            symbol_name(symbols, b.symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirSimpleStmt::Set {
                places,
                op: _,
                value,
            } => {
                for p in pool.places_list(*places) {
                    check_span(p.span)?;
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirSimpleStmt::Expr(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
        }
        Ok(())
    }
}

impl HirExpr {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        if matches!(self.kind, HirExprKind::Error) {
            return Ok(());
        }
        if self.ty == crate::types::TypeInterner::preinterned_error_id() {
            return Err(format!(
                "Expression has Error type at byte {}",
                self.span.start
            ));
        }
        match &self.kind {
            HirExprKind::Path { symbol } => {
                let _sym = symbols.get(*symbol);
            }
            HirExprKind::TypePath {
                type_symbol,
                member_symbol,
                ..
            } => {
                let _t_sym = symbols.get(*type_symbol);
                let _m_sym = symbols.get(*member_symbol);
            }
            HirExprKind::Generic { callee, .. } => {
                pool.expr(*callee).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Field { base, .. } | HirExprKind::SafeField { base, .. } => {
                let base_node = pool.expr(*base);
                base_node.validate_invariants(pool, symbols)?;
                if base_node.ty == crate::types::TypeInterner::preinterned_error_id() {
                    return Err("Field access base expression has Error type".to_string());
                }
            }
            HirExprKind::Index { base, index } | HirExprKind::SafeIndex { base, index } => {
                pool.expr(*base).validate_invariants(pool, symbols)?;
                pool.expr(*index).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Try { expr } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                let callee_node = pool.expr(*callee);
                callee_node.validate_invariants(pool, symbols)?;
                if callee_node.ty == crate::types::TypeInterner::preinterned_error_id() {
                    return Err("Call callee expression has Error type".to_string());
                }
                for &arg in pool.expr_list(*args) {
                    pool.expr(arg).validate_invariants(pool, symbols)?;
                }
                if let Some(tb) = trailing_block {
                    pool.block(*tb).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::ResultCtor { value, .. } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirExprKind::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let _sym = symbols.get(*struct_symbol);
                for f in pool.field_inits_list(*fields) {
                    check_span(f.span)?;
                    pool.expr(f.value).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::Array { items } => {
                for &item in pool.expr_list(*items) {
                    pool.expr(item).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::Lambda { params, body } => {
                for p in pool.lambda_params_list(*params) {
                    check_span(p.span)?;
                    let _sym = symbols.get(p.symbol);
                }
                match body {
                    HirLambdaBody::Expr(e) => pool.expr(*e).validate_invariants(pool, symbols)?,
                    HirLambdaBody::Block(b) => pool.block(*b).validate_invariants(pool, symbols)?,
                }
            }
            HirExprKind::Alloc { expr } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::AsyncBlock { block } | HirExprKind::UnsafeBlock { block } => {
                pool.block(*block).validate_invariants(pool, symbols)?;
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*then_block).validate_invariants(pool, symbols)?;
                pool.block(*else_block).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Match { value, arms } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
                for arm in pool.match_arms_list(*arms) {
                    check_span(arm.span)?;
                    if let Some(g) = &arm.guard {
                        pool.expr(*g).validate_invariants(pool, symbols)?;
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            pool.expr(*e).validate_invariants(pool, symbols)?
                        }
                        HirMatchArmBody::Block(b) => {
                            pool.block(*b).validate_invariants(pool, symbols)?
                        }
                    }
                }
            }
            HirExprKind::Catch { expr, handler } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
                match handler {
                    HirCatchHandler::Expr(e) => pool.expr(*e).validate_invariants(pool, symbols)?,
                    HirCatchHandler::Block { block, .. } => {
                        pool.block(*block).validate_invariants(pool, symbols)?
                    }
                }
            }
            HirExprKind::NullCoalesce { left, right } => {
                pool.expr(*left).validate_invariants(pool, symbols)?;
                pool.expr(*right).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Cast { expr, .. } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Unary { expr, .. } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Binary { left, right, .. } => {
                pool.expr(*left).validate_invariants(pool, symbols)?;
                pool.expr(*right).validate_invariants(pool, symbols)?;
            }
            HirExprKind::StringInterp { parts } => {
                for part in parts {
                    if let HirStringPart::Expr(e) = part {
                        pool.expr(*e).validate_invariants(pool, symbols)?;
                    }
                }
            }
            HirExprKind::ToStr { value } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::Bool(_)
            | HirExprKind::Char(_)
            | HirExprKind::Str(_)
            | HirExprKind::Nil
            | HirExprKind::Error => {}
        }
        Ok(())
    }
}

mod pool;
