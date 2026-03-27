//! Lexer for GratiaScript — tokenizes source code into a stream of tokens.

use crate::error::CompileError;
use crate::token::{Span, Token, TokenKind};

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Lexer {
            source: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Tokenize the entire source, returning all tokens including EOF.
    pub fn tokenize(&mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof { break; }
        }
        Ok(tokens)
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.pos).copied()
    }

    fn peek_ahead(&self, offset: usize) -> Option<char> {
        self.source.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.source.get(self.pos).copied()?;
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn span_here(&self) -> Span {
        Span::new(self.pos, self.pos, self.line, self.col)
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(ch) = self.advance() {
            if ch == '\n' { break; }
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), CompileError> {
        let (line, col) = (self.line, self.col);
        loop {
            match self.advance() {
                None => return Err(CompileError::lexer("unterminated block comment", line, col)),
                Some('*') if self.peek() == Some('/') => {
                    self.advance();
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    fn next_token(&mut self) -> Result<Token, CompileError> {
        loop {
            self.skip_whitespace();

            let start = self.pos;
            let line = self.line;
            let col = self.col;

            let ch = match self.peek() {
                Some(ch) => ch,
                None => return Ok(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(start, start, line, col),
                }),
            };

            // Comments
            if ch == '/' {
                if self.peek_ahead(1) == Some('/') {
                    self.advance(); self.advance();
                    self.skip_line_comment();
                    continue;
                }
                if self.peek_ahead(1) == Some('*') {
                    self.advance(); self.advance();
                    self.skip_block_comment()?;
                    continue;
                }
            }

            // @ builtins
            if ch == '@' {
                return self.lex_at_builtin();
            }

            // Numbers
            if ch.is_ascii_digit() {
                return self.lex_number();
            }

            // Strings
            if ch == '"' {
                return self.lex_string();
            }

            // Identifiers / keywords
            if ch.is_ascii_alphabetic() || ch == '_' {
                return self.lex_ident();
            }

            // Operators and punctuation
            return self.lex_operator();
        }
    }

    fn lex_at_builtin(&mut self) -> Result<Token, CompileError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        self.advance(); // consume @

        let mut name = String::from("@");
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                name.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        // Handle @store.read / @store.write
        if name == "@store" && self.peek() == Some('.') {
            self.advance(); // consume .
            name.push('.');
            while let Some(ch) = self.peek() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    name.push(ch);
                    self.advance();
                } else {
                    break;
                }
            }
        }

        match TokenKind::builtin(&name) {
            Some(kind) => Ok(Token {
                kind,
                span: Span::new(start, self.pos, line, col),
            }),
            None => Err(CompileError::lexer(
                format!("unknown builtin: {}", name),
                line, col,
            )),
        }
    }

    fn lex_number(&mut self) -> Result<Token, CompileError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        let mut s = String::new();
        let mut is_float = false;

        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '_' {
                if ch != '_' { s.push(ch); }
                self.advance();
            } else if ch == '.' && !is_float {
                // WHY: Check next char to distinguish 1.0 (float) from 1.method() (int + dot)
                if self.peek_ahead(1).map_or(false, |c| c.is_ascii_digit()) {
                    is_float = true;
                    s.push(ch);
                    self.advance();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        let span = Span::new(start, self.pos, line, col);
        if is_float {
            let val: f64 = s.parse().map_err(|_| CompileError::lexer("invalid float", line, col))?;
            Ok(Token { kind: TokenKind::FloatLiteral(val), span })
        } else {
            let val: i64 = s.parse().map_err(|_| CompileError::lexer("invalid integer", line, col))?;
            Ok(Token { kind: TokenKind::IntLiteral(val), span })
        }
    }

    fn lex_string(&mut self) -> Result<Token, CompileError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        self.advance(); // consume opening "

        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(CompileError::lexer("unterminated string", line, col)),
                Some('"') => break,
                Some('\\') => {
                    match self.advance() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('\\') => s.push('\\'),
                        Some('"') => s.push('"'),
                        Some(c) => return Err(CompileError::lexer(
                            format!("invalid escape: \\{}", c), self.line, self.col
                        )),
                        None => return Err(CompileError::lexer("unterminated escape", line, col)),
                    }
                }
                Some(c) => s.push(c),
            }
        }

        Ok(Token {
            kind: TokenKind::StringLiteral(s),
            span: Span::new(start, self.pos, line, col),
        })
    }

    fn lex_ident(&mut self) -> Result<Token, CompileError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        let mut s = String::new();

        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                s.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        let kind = TokenKind::keyword(&s).unwrap_or(TokenKind::Ident(s));
        Ok(Token {
            kind,
            span: Span::new(start, self.pos, line, col),
        })
    }

    fn lex_operator(&mut self) -> Result<Token, CompileError> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;
        let ch = self.advance().unwrap();

        let kind = match ch {
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '.' => TokenKind::Dot,
            '=' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::Eq
                } else if self.peek() == Some('>') {
                    self.advance();
                    TokenKind::Arrow
                } else {
                    TokenKind::Assign
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::NotEq
                } else {
                    TokenKind::Not
                }
            }
            '<' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::LtEq
                } else {
                    TokenKind::Lt
                }
            }
            '>' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                }
            }
            '&' => {
                if self.peek() == Some('&') {
                    self.advance();
                    TokenKind::And
                } else {
                    TokenKind::BitAnd
                }
            }
            '|' => {
                if self.peek() == Some('|') {
                    self.advance();
                    TokenKind::Or
                } else {
                    TokenKind::BitOr
                }
            }
            '^' => TokenKind::BitXor,
            _ => return Err(CompileError::lexer(
                format!("unexpected character: '{}'", ch), line, col
            )),
        };

        Ok(Token {
            kind,
            span: Span::new(start, self.pos, line, col),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<TokenKind> {
        Lexer::new(s).tokenize().unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_empty() {
        assert_eq!(lex(""), vec![TokenKind::Eof]);
    }

    #[test]
    fn test_keywords() {
        let tokens = lex("contract function let const if else while for return emit");
        assert!(matches!(tokens[0], TokenKind::Contract));
        assert!(matches!(tokens[1], TokenKind::Function));
        assert!(matches!(tokens[2], TokenKind::Let));
        assert!(matches!(tokens[3], TokenKind::Const));
        assert!(matches!(tokens[4], TokenKind::If));
    }

    #[test]
    fn test_types() {
        let tokens = lex("i32 i64 f32 f64 bool string void address");
        assert!(matches!(tokens[0], TokenKind::TypeI32));
        assert!(matches!(tokens[7], TokenKind::TypeAddress));
    }

    #[test]
    fn test_builtins() {
        let tokens = lex("@location @proximity @presence @sensor @blockHeight @store.read @store.write");
        assert!(matches!(tokens[0], TokenKind::AtLocation));
        assert!(matches!(tokens[1], TokenKind::AtProximity));
        assert!(matches!(tokens[2], TokenKind::AtPresence));
        assert!(matches!(tokens[3], TokenKind::AtSensor));
        assert!(matches!(tokens[5], TokenKind::AtStoreRead));
        assert!(matches!(tokens[6], TokenKind::AtStoreWrite));
    }

    #[test]
    fn test_numbers() {
        let tokens = lex("42 3.14 1_000_000");
        assert_eq!(tokens[0], TokenKind::IntLiteral(42));
        assert_eq!(tokens[1], TokenKind::FloatLiteral(3.14));
        assert_eq!(tokens[2], TokenKind::IntLiteral(1000000));
    }

    #[test]
    fn test_string() {
        let tokens = lex(r#""hello world" "line\nnewline""#);
        assert_eq!(tokens[0], TokenKind::StringLiteral("hello world".into()));
        assert_eq!(tokens[1], TokenKind::StringLiteral("line\nnewline".into()));
    }

    #[test]
    fn test_operators() {
        let tokens = lex("+ - * / == != <= >= && || = =>");
        assert!(matches!(tokens[0], TokenKind::Plus));
        assert!(matches!(tokens[4], TokenKind::Eq));
        assert!(matches!(tokens[5], TokenKind::NotEq));
        assert!(matches!(tokens[8], TokenKind::And));
        assert!(matches!(tokens[10], TokenKind::Assign));
        assert!(matches!(tokens[11], TokenKind::Arrow));
    }

    #[test]
    fn test_comments() {
        let tokens = lex("42 // this is a comment\n43");
        assert_eq!(tokens[0], TokenKind::IntLiteral(42));
        assert_eq!(tokens[1], TokenKind::IntLiteral(43));
    }

    #[test]
    fn test_block_comment() {
        let tokens = lex("42 /* block */ 43");
        assert_eq!(tokens[0], TokenKind::IntLiteral(42));
        assert_eq!(tokens[1], TokenKind::IntLiteral(43));
    }

    #[test]
    fn test_booleans() {
        let tokens = lex("true false");
        assert_eq!(tokens[0], TokenKind::BoolLiteral(true));
        assert_eq!(tokens[1], TokenKind::BoolLiteral(false));
    }

    #[test]
    fn test_contract_snippet() {
        let tokens = lex("contract MyToken { function balance(): i64 { return 0; } }");
        assert!(matches!(tokens[0], TokenKind::Contract));
        assert!(matches!(tokens[1], TokenKind::Ident(_)));
        assert!(matches!(tokens[2], TokenKind::LBrace));
        assert!(matches!(tokens[3], TokenKind::Function));
    }

    #[test]
    fn test_at_location_call() {
        let tokens = lex("let pos = @location();");
        assert!(matches!(tokens[0], TokenKind::Let));
        assert!(matches!(tokens[2], TokenKind::Assign));
        assert!(matches!(tokens[3], TokenKind::AtLocation));
        assert!(matches!(tokens[4], TokenKind::LParen));
        assert!(matches!(tokens[5], TokenKind::RParen));
    }
}
