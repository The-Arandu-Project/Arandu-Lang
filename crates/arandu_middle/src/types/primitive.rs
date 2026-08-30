use std::fmt;

// ── Primitive types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Int,
    Uint,
    Float,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Bool,
    Byte,
    Char,
    Str,
    Any,
}

impl Primitive {
    #[must_use]
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Primitive::Int
                | Primitive::Uint
                | Primitive::Float
                | Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::F32
                | Primitive::F64
                | Primitive::Byte
        )
    }

    #[must_use]
    pub fn is_integer(self) -> bool {
        matches!(
            self,
            Primitive::Int
                | Primitive::Uint
                | Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::Byte
        )
    }

    #[must_use]
    pub fn is_float(self) -> bool {
        matches!(self, Primitive::Float | Primitive::F32 | Primitive::F64)
    }

    #[must_use]
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            Primitive::Int
                | Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::Float
                | Primitive::F32
                | Primitive::F64
        )
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Primitive::Int => "int",
            Primitive::Uint => "uint",
            Primitive::Float => "float",
            Primitive::I8 => "i8",
            Primitive::I16 => "i16",
            Primitive::I32 => "i32",
            Primitive::I64 => "i64",
            Primitive::U8 => "u8",
            Primitive::U16 => "u16",
            Primitive::U32 => "u32",
            Primitive::U64 => "u64",
            Primitive::F32 => "f32",
            Primitive::F64 => "f64",
            Primitive::Bool => "bool",
            Primitive::Byte => "byte",
            Primitive::Char => "char",
            Primitive::Str => "str",
            Primitive::Any => "any",
        }
    }

    /// Parse a primitive type name from an AST `TypeExpr::Primitive`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Primitive> {
        match name {
            "int" => Some(Primitive::Int),
            "uint" => Some(Primitive::Uint),
            "float" => Some(Primitive::Float),
            "i8" => Some(Primitive::I8),
            "i16" => Some(Primitive::I16),
            "i32" => Some(Primitive::I32),
            "i64" => Some(Primitive::I64),
            "u8" => Some(Primitive::U8),
            "u16" => Some(Primitive::U16),
            "u32" => Some(Primitive::U32),
            "u64" => Some(Primitive::U64),
            "f32" => Some(Primitive::F32),
            "f64" => Some(Primitive::F64),
            "bool" => Some(Primitive::Bool),
            "byte" => Some(Primitive::Byte),
            "char" => Some(Primitive::Char),
            "str" => Some(Primitive::Str),
            "any" => Some(Primitive::Any),
            _ => None,
        }
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_numeric_int() {
        assert!(Primitive::Int.is_numeric());
        assert!(Primitive::Uint.is_numeric());
        assert!(Primitive::Float.is_numeric());
        assert!(Primitive::I8.is_numeric());
        assert!(Primitive::I16.is_numeric());
        assert!(Primitive::I32.is_numeric());
        assert!(Primitive::I64.is_numeric());
        assert!(Primitive::U8.is_numeric());
        assert!(Primitive::U16.is_numeric());
        assert!(Primitive::U32.is_numeric());
        assert!(Primitive::U64.is_numeric());
        assert!(Primitive::F32.is_numeric());
        assert!(Primitive::F64.is_numeric());
        assert!(Primitive::Byte.is_numeric());
    }

    #[test]
    fn non_numeric_types() {
        assert!(!Primitive::Bool.is_numeric());
        assert!(!Primitive::Char.is_numeric());
        assert!(!Primitive::Str.is_numeric());
        assert!(!Primitive::Any.is_numeric());
    }

    #[test]
    fn is_integer() {
        assert!(Primitive::Int.is_integer());
        assert!(Primitive::Uint.is_integer());
        assert!(Primitive::I8.is_integer());
        assert!(Primitive::U8.is_integer());
        assert!(!Primitive::Float.is_integer());
        assert!(!Primitive::F32.is_integer());
        assert!(!Primitive::Bool.is_integer());
        assert!(!Primitive::Str.is_integer());
    }

    #[test]
    fn is_float() {
        assert!(Primitive::Float.is_float());
        assert!(Primitive::F32.is_float());
        assert!(Primitive::F64.is_float());
        assert!(!Primitive::Int.is_float());
        assert!(!Primitive::Bool.is_float());
    }

    #[test]
    fn is_signed() {
        assert!(Primitive::Int.is_signed());
        assert!(Primitive::I8.is_signed());
        assert!(Primitive::I16.is_signed());
        assert!(Primitive::I32.is_signed());
        assert!(Primitive::I64.is_signed());
        assert!(Primitive::Float.is_signed());
        assert!(Primitive::F32.is_signed());
        assert!(Primitive::F64.is_signed());
        assert!(!Primitive::Uint.is_signed());
        assert!(!Primitive::U8.is_signed());
        assert!(!Primitive::Bool.is_signed());
    }

    #[test]
    fn as_str_all_primitives() {
        assert_eq!(Primitive::Int.as_str(), "int");
        assert_eq!(Primitive::Uint.as_str(), "uint");
        assert_eq!(Primitive::Float.as_str(), "float");
        assert_eq!(Primitive::I8.as_str(), "i8");
        assert_eq!(Primitive::I16.as_str(), "i16");
        assert_eq!(Primitive::I32.as_str(), "i32");
        assert_eq!(Primitive::I64.as_str(), "i64");
        assert_eq!(Primitive::U8.as_str(), "u8");
        assert_eq!(Primitive::U16.as_str(), "u16");
        assert_eq!(Primitive::U32.as_str(), "u32");
        assert_eq!(Primitive::U64.as_str(), "u64");
        assert_eq!(Primitive::F32.as_str(), "f32");
        assert_eq!(Primitive::F64.as_str(), "f64");
        assert_eq!(Primitive::Bool.as_str(), "bool");
        assert_eq!(Primitive::Byte.as_str(), "byte");
        assert_eq!(Primitive::Char.as_str(), "char");
        assert_eq!(Primitive::Str.as_str(), "str");
        assert_eq!(Primitive::Any.as_str(), "any");
    }

    #[test]
    fn from_name_all_valid() {
        assert_eq!(Primitive::from_name("int"), Some(Primitive::Int));
        assert_eq!(Primitive::from_name("uint"), Some(Primitive::Uint));
        assert_eq!(Primitive::from_name("float"), Some(Primitive::Float));
        assert_eq!(Primitive::from_name("i8"), Some(Primitive::I8));
        assert_eq!(Primitive::from_name("i16"), Some(Primitive::I16));
        assert_eq!(Primitive::from_name("i32"), Some(Primitive::I32));
        assert_eq!(Primitive::from_name("i64"), Some(Primitive::I64));
        assert_eq!(Primitive::from_name("u8"), Some(Primitive::U8));
        assert_eq!(Primitive::from_name("u16"), Some(Primitive::U16));
        assert_eq!(Primitive::from_name("u32"), Some(Primitive::U32));
        assert_eq!(Primitive::from_name("u64"), Some(Primitive::U64));
        assert_eq!(Primitive::from_name("f32"), Some(Primitive::F32));
        assert_eq!(Primitive::from_name("f64"), Some(Primitive::F64));
        assert_eq!(Primitive::from_name("bool"), Some(Primitive::Bool));
        assert_eq!(Primitive::from_name("byte"), Some(Primitive::Byte));
        assert_eq!(Primitive::from_name("char"), Some(Primitive::Char));
        assert_eq!(Primitive::from_name("str"), Some(Primitive::Str));
        assert_eq!(Primitive::from_name("any"), Some(Primitive::Any));
    }

    #[test]
    fn from_name_invalid_returns_none() {
        assert_eq!(Primitive::from_name("InvalidType"), None);
        assert_eq!(Primitive::from_name(""), None);
        assert_eq!(Primitive::from_name("int64"), None);
    }

    #[test]
    fn display_primitives() {
        assert_eq!(format!("{}", Primitive::Int), "int");
        assert_eq!(format!("{}", Primitive::Bool), "bool");
        assert_eq!(format!("{}", Primitive::Str), "str");
        assert_eq!(format!("{}", Primitive::Float), "float");
    }
}
