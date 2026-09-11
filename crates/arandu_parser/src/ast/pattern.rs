use super::{Block, ExprId, IndexRange, PatternId, TypeName};
use arandu_lexer::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub span: Span,
    pub pattern: PatternId,
    pub guard: Option<ExprId>,
    pub body: MatchArmBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchArmBody {
    Expr { span: Span, expr: ExprId },
    Block { span: Span, block: Block },
}

use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    Bind {
        span: Span,
        name: SmolStr,
        mutable: bool,
    },
    Literal {
        span: Span,
        expr: ExprId,
    },
    Enum {
        span: Span,
        type_name: TypeName,
        variant: SmolStr,
        payload: IndexRange, // IndexRange referencing PatternId
    },
    TypeTuple {
        span: Span,
        name: SmolStr,
        payload: IndexRange, // IndexRange referencing PatternId
    },
    Struct {
        span: Span,
        type_name: TypeName,
        fields: IndexRange, // IndexRange referencing FieldPatternId
    },
    Tuple {
        span: Span,
        items: IndexRange, // IndexRange referencing PatternId
    },
    Range {
        span: Span,
        start: ExprId,
        inclusive: bool,
        end: ExprId,
    },
    /// SYN.4: `p1 | p2 | …` alternatives (same bindings policy as typeck).
    Or {
        span: Span,
        alts: IndexRange, // PatternId
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldPattern {
    pub span: Span,
    pub name: SmolStr,
    pub pattern: Option<PatternId>,
}

impl Pattern {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard { span }
            | Pattern::Bind { span, .. }
            | Pattern::Literal { span, .. }
            | Pattern::Enum { span, .. }
            | Pattern::TypeTuple { span, .. }
            | Pattern::Struct { span, .. }
            | Pattern::Tuple { span, .. }
            | Pattern::Range { span, .. }
            | Pattern::Or { span, .. } => *span,
        }
    }
}
