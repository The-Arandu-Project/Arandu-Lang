//! Stable operator enums shared by HIR and AMIR (decoupled from the parser AST).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Await,
    /// Shared address-of: `&expr` (F2.0).
    Ref,
    /// Exclusive address-of: `&mut expr` (F2.0).
    RefMut,
    /// Dereference `*expr` (safe for refs; raw ptr needs unsafe).
    Deref,
}

impl UnaryOp {
    /// Stable, versioned hashing tag. Keep exhaustive so new operators cannot
    /// silently reuse an existing codegen cache entry.
    #[must_use]
    pub const fn stable_tag(self) -> u8 {
        match self {
            Self::Neg => 0,
            Self::Not => 1,
            Self::BitNot => 2,
            Self::Await => 3,
            Self::Ref => 4,
            Self::RefMut => 5,
            Self::Deref => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BinaryOp {
    Or,
    And,
    Equal,
    NotEqual,
    Lt,
    Gt,
    LtEqual,
    GtEqual,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitOr,
    BitXor,
    BitAnd,
    ShiftLeft,
    ShiftRight,
    NullCoalesce,
    RangeExclusive,
    RangeInclusive,
}

impl BinaryOp {
    /// Stable, versioned hashing tag. Keep exhaustive so new operators cannot
    /// silently reuse an existing codegen cache entry.
    #[must_use]
    pub const fn stable_tag(self) -> u8 {
        match self {
            Self::Or => 0,
            Self::And => 1,
            Self::Equal => 2,
            Self::NotEqual => 3,
            Self::Lt => 4,
            Self::Gt => 5,
            Self::LtEqual => 6,
            Self::GtEqual => 7,
            Self::Add => 8,
            Self::Sub => 9,
            Self::Mul => 10,
            Self::Div => 11,
            Self::Mod => 12,
            Self::BitOr => 13,
            Self::BitXor => 14,
            Self::BitAnd => 15,
            Self::ShiftLeft => 16,
            Self::ShiftRight => 17,
            Self::NullCoalesce => 18,
            Self::RangeExclusive => 19,
            Self::RangeInclusive => 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SetOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShiftLeftAssign,
    ShiftRightAssign,
}

impl From<arandu_parser::UnaryOp> for UnaryOp {
    fn from(op: arandu_parser::UnaryOp) -> Self {
        match op {
            arandu_parser::UnaryOp::Neg => Self::Neg,
            arandu_parser::UnaryOp::Not => Self::Not,
            arandu_parser::UnaryOp::BitNot => Self::BitNot,
            arandu_parser::UnaryOp::Await => Self::Await,
            arandu_parser::UnaryOp::Ref => Self::Ref,
            arandu_parser::UnaryOp::RefMut => Self::RefMut,
            arandu_parser::UnaryOp::Deref => Self::Deref,
        }
    }
}

impl From<arandu_parser::BinaryOp> for BinaryOp {
    fn from(op: arandu_parser::BinaryOp) -> Self {
        match op {
            arandu_parser::BinaryOp::Or => Self::Or,
            arandu_parser::BinaryOp::And => Self::And,
            arandu_parser::BinaryOp::Equal => Self::Equal,
            arandu_parser::BinaryOp::NotEqual => Self::NotEqual,
            arandu_parser::BinaryOp::Lt => Self::Lt,
            arandu_parser::BinaryOp::Gt => Self::Gt,
            arandu_parser::BinaryOp::LtEqual => Self::LtEqual,
            arandu_parser::BinaryOp::GtEqual => Self::GtEqual,
            arandu_parser::BinaryOp::Add => Self::Add,
            arandu_parser::BinaryOp::Sub => Self::Sub,
            arandu_parser::BinaryOp::Mul => Self::Mul,
            arandu_parser::BinaryOp::Div => Self::Div,
            arandu_parser::BinaryOp::Mod => Self::Mod,
            arandu_parser::BinaryOp::BitOr => Self::BitOr,
            arandu_parser::BinaryOp::BitXor => Self::BitXor,
            arandu_parser::BinaryOp::BitAnd => Self::BitAnd,
            arandu_parser::BinaryOp::ShiftLeft => Self::ShiftLeft,
            arandu_parser::BinaryOp::ShiftRight => Self::ShiftRight,
            arandu_parser::BinaryOp::NullCoalesce => Self::NullCoalesce,
            arandu_parser::BinaryOp::RangeExclusive => Self::RangeExclusive,
            arandu_parser::BinaryOp::RangeInclusive => Self::RangeInclusive,
        }
    }
}

impl From<arandu_parser::SetOp> for SetOp {
    fn from(op: arandu_parser::SetOp) -> Self {
        match op {
            arandu_parser::SetOp::Assign => Self::Assign,
            arandu_parser::SetOp::AddAssign => Self::AddAssign,
            arandu_parser::SetOp::SubAssign => Self::SubAssign,
            arandu_parser::SetOp::MulAssign => Self::MulAssign,
            arandu_parser::SetOp::DivAssign => Self::DivAssign,
            arandu_parser::SetOp::ModAssign => Self::ModAssign,
            arandu_parser::SetOp::BitAndAssign => Self::BitAndAssign,
            arandu_parser::SetOp::BitOrAssign => Self::BitOrAssign,
            arandu_parser::SetOp::BitXorAssign => Self::BitXorAssign,
            arandu_parser::SetOp::ShiftLeftAssign => Self::ShiftLeftAssign,
            arandu_parser::SetOp::ShiftRightAssign => Self::ShiftRightAssign,
        }
    }
}
