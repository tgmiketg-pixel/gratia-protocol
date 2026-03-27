//! Compiler error types for GratiaScript.

use crate::token::Span;

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("Lexer error at line {line}, col {col}: {message}")]
    LexerError {
        message: String,
        line: usize,
        col: usize,
    },

    #[error("Parse error at line {line}, col {col}: {message}")]
    ParseError {
        message: String,
        line: usize,
        col: usize,
    },

    #[error("Type error at line {line}, col {col}: {message}")]
    TypeError {
        message: String,
        line: usize,
        col: usize,
    },

    #[error("Code generation error: {message}")]
    CodegenError {
        message: String,
    },
}

impl CompileError {
    pub fn lexer(msg: impl Into<String>, line: usize, col: usize) -> Self {
        CompileError::LexerError { message: msg.into(), line, col }
    }

    pub fn parse(msg: impl Into<String>, span: &Span) -> Self {
        CompileError::ParseError { message: msg.into(), line: span.line, col: span.col }
    }

    pub fn parse_at(msg: impl Into<String>, line: usize, col: usize) -> Self {
        CompileError::ParseError { message: msg.into(), line, col }
    }

    pub fn type_err(msg: impl Into<String>, line: usize, col: usize) -> Self {
        CompileError::TypeError { message: msg.into(), line, col }
    }

    pub fn codegen(msg: impl Into<String>) -> Self {
        CompileError::CodegenError { message: msg.into() }
    }
}
