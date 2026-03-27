//! Token definitions for the GratiaScript lexer.
//!
//! GratiaScript uses TypeScript-derived syntax with mobile-native extensions.
//! Tokens represent the smallest meaningful units in the source code.

/// Source location for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(start: usize, end: usize, line: usize, col: usize) -> Self {
        Span { start, end, line, col }
    }
}

/// A token with its span in the source code.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// All token types in GratiaScript.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ── Literals ──
    IntLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),
    BoolLiteral(bool),

    // ── Identifiers ──
    Ident(String),

    // ── Keywords ──
    Contract,
    Function,
    Let,
    Const,
    If,
    Else,
    While,
    For,
    Return,
    Emit,
    Import,

    // ── Types ──
    TypeI32,
    TypeI64,
    TypeF32,
    TypeF64,
    TypeBool,
    TypeString,
    TypeBytes,
    TypeVoid,
    TypeAddress,

    // ── Mobile-native builtins (the moat) ──
    /// @location() — GPS coordinates
    AtLocation,
    /// @proximity() — Nearby peer count
    AtProximity,
    /// @presence() — Composite Presence Score (40-100)
    AtPresence,
    /// @sensor(type) — Sensor readings
    AtSensor,
    /// @blockHeight() — Current block height
    AtBlockHeight,
    /// @blockTime() — Current block timestamp
    AtBlockTime,
    /// @caller() — Caller's address
    AtCaller,
    /// @balance() — Caller's balance in Lux
    AtBalance,
    /// @store.read(key) — Read from contract storage
    AtStoreRead,
    /// @store.write(key, value) — Write to contract storage
    AtStoreWrite,

    // ── Operators ──
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    Not,
    BitAnd,
    BitOr,
    BitXor,

    // ── Delimiters ──
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // ── Punctuation ──
    Comma,
    Colon,
    Semicolon,
    Arrow, // =>
    Dot,

    // ── Special ──
    Eof,
}

impl TokenKind {
    /// Check if a string is a keyword and return the corresponding token kind.
    pub fn keyword(s: &str) -> Option<TokenKind> {
        match s {
            "contract" => Some(TokenKind::Contract),
            "function" => Some(TokenKind::Function),
            "let" => Some(TokenKind::Let),
            "const" => Some(TokenKind::Const),
            "if" => Some(TokenKind::If),
            "else" => Some(TokenKind::Else),
            "while" => Some(TokenKind::While),
            "for" => Some(TokenKind::For),
            "return" => Some(TokenKind::Return),
            "emit" => Some(TokenKind::Emit),
            "import" => Some(TokenKind::Import),
            "true" => Some(TokenKind::BoolLiteral(true)),
            "false" => Some(TokenKind::BoolLiteral(false)),
            "i32" => Some(TokenKind::TypeI32),
            "i64" => Some(TokenKind::TypeI64),
            "f32" => Some(TokenKind::TypeF32),
            "f64" => Some(TokenKind::TypeF64),
            "bool" => Some(TokenKind::TypeBool),
            "string" => Some(TokenKind::TypeString),
            "bytes" => Some(TokenKind::TypeBytes),
            "void" => Some(TokenKind::TypeVoid),
            "address" => Some(TokenKind::TypeAddress),
            _ => None,
        }
    }

    /// Check if a @-prefixed identifier is a builtin.
    pub fn builtin(s: &str) -> Option<TokenKind> {
        match s {
            "@location" => Some(TokenKind::AtLocation),
            "@proximity" => Some(TokenKind::AtProximity),
            "@presence" => Some(TokenKind::AtPresence),
            "@sensor" => Some(TokenKind::AtSensor),
            "@blockHeight" => Some(TokenKind::AtBlockHeight),
            "@blockTime" => Some(TokenKind::AtBlockTime),
            "@caller" => Some(TokenKind::AtCaller),
            "@balance" => Some(TokenKind::AtBalance),
            "@store.read" => Some(TokenKind::AtStoreRead),
            "@store.write" => Some(TokenKind::AtStoreWrite),
            _ => None,
        }
    }
}
