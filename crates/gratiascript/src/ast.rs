//! Abstract Syntax Tree for GratiaScript.
//!
//! The AST represents the structure of a GratiaScript contract after parsing.
//! It is consumed by the type checker and code generator.

use crate::token::Span;

/// A complete GratiaScript contract.
#[derive(Debug, Clone)]
pub struct Contract {
    pub name: String,
    pub fields: Vec<Field>,
    pub functions: Vec<Function>,
    pub span: Span,
}

/// A contract state field (let or const at contract level).
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub mutable: bool,
    pub initializer: Option<Expr>,
    pub span: Span,
}

/// A function definition inside a contract.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// GratiaScript types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    I32,
    I64,
    F32,
    F64,
    Bool,
    String,
    Bytes,
    Void,
    Address,
    /// Location tuple (lat: f32, lon: f32) — returned by @location()
    Location,
}

impl Type {
    /// Whether this type is a numeric type.
    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::I32 | Type::I64 | Type::F32 | Type::F64)
    }

    /// Whether this type is a floating-point type.
    pub fn is_float(&self) -> bool {
        matches!(self, Type::F32 | Type::F64)
    }

    /// Whether this type is an integer type.
    pub fn is_integer(&self) -> bool {
        matches!(self, Type::I32 | Type::I64)
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::I32 => write!(f, "i32"),
            Type::I64 => write!(f, "i64"),
            Type::F32 => write!(f, "f32"),
            Type::F64 => write!(f, "f64"),
            Type::Bool => write!(f, "bool"),
            Type::String => write!(f, "string"),
            Type::Bytes => write!(f, "bytes"),
            Type::Void => write!(f, "void"),
            Type::Address => write!(f, "address"),
            Type::Location => write!(f, "Location"),
        }
    }
}

/// A statement in a function body.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// Variable declaration: let x: i32 = expr;
    VarDecl {
        name: String,
        ty: Option<Type>,
        mutable: bool,
        initializer: Expr,
        span: Span,
    },
    /// Assignment: x = expr;
    Assign {
        target: String,
        value: Expr,
        span: Span,
    },
    /// Return statement: return expr;
    Return {
        value: Option<Expr>,
        span: Span,
    },
    /// If/else: if (cond) { ... } else { ... }
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
        span: Span,
    },
    /// While loop: while (cond) { ... }
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    /// Expression statement (function call, etc.)
    Expr(Expr),
    /// Emit event: emit("topic", "data");
    Emit {
        topic: Expr,
        data: Expr,
        span: Span,
    },
    /// Storage write: @store.write(key, value);
    StoreWrite {
        key: Expr,
        value: Expr,
        span: Span,
    },
}

/// An expression.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal
    IntLit(i64, Span),
    /// Float literal
    FloatLit(f64, Span),
    /// String literal
    StringLit(String, Span),
    /// Boolean literal
    BoolLit(bool, Span),
    /// Variable reference
    Ident(String, Span),
    /// Binary operation: a + b, a == b, etc.
    BinOp {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    /// Unary operation: !x, -x
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    /// Function call: foo(a, b)
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    /// Mobile-native builtin call: @location(), @proximity(), etc.
    BuiltinCall {
        builtin: Builtin,
        args: Vec<Expr>,
        span: Span,
    },
    /// Field access: loc.lat, loc.lon
    FieldAccess {
        object: Box<Expr>,
        field: String,
        span: Span,
    },
    /// Type cast: expr as i32
    Cast {
        expr: Box<Expr>,
        target_type: Type,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> &Span {
        match self {
            Expr::IntLit(_, s) | Expr::FloatLit(_, s) | Expr::StringLit(_, s)
            | Expr::BoolLit(_, s) | Expr::Ident(_, s) => s,
            Expr::BinOp { span, .. } | Expr::UnaryOp { span, .. }
            | Expr::Call { span, .. } | Expr::BuiltinCall { span, .. }
            | Expr::FieldAccess { span, .. } | Expr::Cast { span, .. } => span,
        }
    }
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, NotEq, Lt, Gt, LtEq, GtEq,
    And, Or,
    BitAnd, BitOr, BitXor,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// Mobile-native builtin functions — the competitive moat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// @location() → Location {lat: f32, lon: f32}
    Location,
    /// @proximity() → i32 (nearby peer count)
    Proximity,
    /// @presence() → i32 (Presence Score 40-100)
    Presence,
    /// @sensor(type) → f64 (sensor reading)
    Sensor,
    /// @blockHeight() → i64
    BlockHeight,
    /// @blockTime() → i64
    BlockTime,
    /// @caller() → address
    Caller,
    /// @balance() → i64 (caller's balance in Lux)
    Balance,
    /// @store.read(key) → bytes
    StoreRead,
}
