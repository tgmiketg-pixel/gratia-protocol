//! Parser for GratiaScript — transforms tokens into an AST.
//!
//! Uses recursive descent parsing with operator precedence climbing
//! for expressions. The grammar is TypeScript-inspired but simplified
//! for smart contract use cases.

use crate::ast::*;
use crate::error::CompileError;
use crate::token::{Span, Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Parse a complete GratiaScript contract.
    pub fn parse_contract(&mut self) -> Result<Contract, CompileError> {
        let span = self.current_span();
        self.expect(TokenKind::Contract)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut fields = Vec::new();
        let mut functions = Vec::new();

        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            if self.check(TokenKind::Function) {
                functions.push(self.parse_function()?);
            } else if self.check(TokenKind::Let) || self.check(TokenKind::Const) {
                fields.push(self.parse_field()?);
            } else {
                return Err(CompileError::parse(
                    "expected 'function', 'let', or 'const'",
                    &self.current_span(),
                ));
            }
        }
        self.expect(TokenKind::RBrace)?;

        Ok(Contract { name, fields, functions, span })
    }

    fn parse_field(&mut self) -> Result<Field, CompileError> {
        let span = self.current_span();
        let mutable = self.check(TokenKind::Let);
        self.advance(); // consume let/const

        let name = self.expect_ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.parse_type()?;

        let initializer = if self.try_consume(TokenKind::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon)?;

        Ok(Field { name, ty, mutable, initializer, span })
    }

    fn parse_function(&mut self) -> Result<Function, CompileError> {
        let span = self.current_span();
        self.expect(TokenKind::Function)?;
        let name = self.expect_ident()?;

        // Parameters
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while !self.check(TokenKind::RParen) {
            if !params.is_empty() {
                self.expect(TokenKind::Comma)?;
            }
            let pspan = self.current_span();
            let pname = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let pty = self.parse_type()?;
            params.push(Param { name: pname, ty: pty, span: pspan });
        }
        self.expect(TokenKind::RParen)?;

        // Return type
        let return_type = if self.try_consume(TokenKind::Colon) {
            self.parse_type()?
        } else {
            Type::Void
        };

        // Body
        self.expect(TokenKind::LBrace)?;
        let body = self.parse_block()?;
        self.expect(TokenKind::RBrace)?;

        Ok(Function { name, params, return_type, body, span })
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, CompileError> {
        let mut stmts = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.check(TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, CompileError> {
        match self.current_kind() {
            TokenKind::Let | TokenKind::Const => self.parse_var_decl(),
            TokenKind::Return => self.parse_return(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Emit => self.parse_emit(),
            TokenKind::AtStoreWrite => self.parse_store_write(),
            _ => {
                // Could be assignment (ident = ...) or expression statement
                if self.is_assignment() {
                    self.parse_assignment()
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(TokenKind::Semicolon)?;
                    Ok(Stmt::Expr(expr))
                }
            }
        }
    }

    fn parse_var_decl(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        let mutable = self.check(TokenKind::Let);
        self.advance();

        let name = self.expect_ident()?;
        let ty = if self.try_consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(TokenKind::Assign)?;
        let initializer = self.parse_expr()?;
        self.expect(TokenKind::Semicolon)?;

        Ok(Stmt::VarDecl { name, ty, mutable, initializer, span })
    }

    fn parse_return(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        self.advance(); // consume 'return'

        let value = if !self.check(TokenKind::Semicolon) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon)?;

        Ok(Stmt::Return { value, span })
    }

    fn parse_if(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        self.advance(); // consume 'if'

        self.expect(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;

        self.expect(TokenKind::LBrace)?;
        let then_body = self.parse_block()?;
        self.expect(TokenKind::RBrace)?;

        let else_body = if self.try_consume(TokenKind::Else) {
            if self.check(TokenKind::If) {
                // else if
                vec![self.parse_if()?]
            } else {
                self.expect(TokenKind::LBrace)?;
                let body = self.parse_block()?;
                self.expect(TokenKind::RBrace)?;
                body
            }
        } else {
            vec![]
        };

        Ok(Stmt::If { condition, then_body, else_body, span })
    }

    fn parse_while(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        self.advance(); // consume 'while'

        self.expect(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;

        self.expect(TokenKind::LBrace)?;
        let body = self.parse_block()?;
        self.expect(TokenKind::RBrace)?;

        Ok(Stmt::While { condition, body, span })
    }

    fn parse_emit(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        self.advance(); // consume 'emit'

        self.expect(TokenKind::LParen)?;
        let topic = self.parse_expr()?;
        self.expect(TokenKind::Comma)?;
        let data = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::Semicolon)?;

        Ok(Stmt::Emit { topic, data, span })
    }

    fn parse_store_write(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        self.advance(); // consume @store.write

        self.expect(TokenKind::LParen)?;
        let key = self.parse_expr()?;
        self.expect(TokenKind::Comma)?;
        let value = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::Semicolon)?;

        Ok(Stmt::StoreWrite { key, value, span })
    }

    fn is_assignment(&self) -> bool {
        matches!(self.current_kind(), TokenKind::Ident(_))
            && self.peek_kind() == Some(&TokenKind::Assign)
    }

    fn parse_assignment(&mut self) -> Result<Stmt, CompileError> {
        let span = self.current_span();
        let target = self.expect_ident()?;
        self.expect(TokenKind::Assign)?;
        let value = self.parse_expr()?;
        self.expect(TokenKind::Semicolon)?;

        Ok(Stmt::Assign { target, value, span })
    }

    // ========================================================================
    // Expression parsing — precedence climbing
    // ========================================================================

    fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_and()?;
        while self.try_consume(TokenKind::Or) {
            let right = self.parse_and()?;
            let span = *left.span();
            left = Expr::BinOp { op: BinOp::Or, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_equality()?;
        while self.try_consume(TokenKind::And) {
            let right = self.parse_equality()?;
            let span = *left.span();
            left = Expr::BinOp { op: BinOp::And, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = if self.try_consume(TokenKind::Eq) { BinOp::Eq }
                else if self.try_consume(TokenKind::NotEq) { BinOp::NotEq }
                else { break };
            let right = self.parse_comparison()?;
            let span = *left.span();
            left = Expr::BinOp { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = if self.try_consume(TokenKind::Lt) { BinOp::Lt }
                else if self.try_consume(TokenKind::Gt) { BinOp::Gt }
                else if self.try_consume(TokenKind::LtEq) { BinOp::LtEq }
                else if self.try_consume(TokenKind::GtEq) { BinOp::GtEq }
                else { break };
            let right = self.parse_additive()?;
            let span = *left.span();
            left = Expr::BinOp { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = if self.try_consume(TokenKind::Plus) { BinOp::Add }
                else if self.try_consume(TokenKind::Minus) { BinOp::Sub }
                else { break };
            let right = self.parse_multiplicative()?;
            let span = *left.span();
            left = Expr::BinOp { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, CompileError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = if self.try_consume(TokenKind::Star) { BinOp::Mul }
                else if self.try_consume(TokenKind::Slash) { BinOp::Div }
                else if self.try_consume(TokenKind::Percent) { BinOp::Mod }
                else { break };
            let right = self.parse_unary()?;
            let span = *left.span();
            left = Expr::BinOp { op, left: Box::new(left), right: Box::new(right), span };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        if self.try_consume(TokenKind::Not) {
            let span = self.current_span();
            let operand = self.parse_unary()?;
            return Ok(Expr::UnaryOp { op: UnaryOp::Not, operand: Box::new(operand), span });
        }
        if self.try_consume(TokenKind::Minus) {
            let span = self.current_span();
            let operand = self.parse_unary()?;
            return Ok(Expr::UnaryOp { op: UnaryOp::Neg, operand: Box::new(operand), span });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, CompileError> {
        let mut expr = self.parse_primary()?;

        // Handle field access: expr.field
        while self.try_consume(TokenKind::Dot) {
            let span = self.current_span();
            let field = self.expect_ident()?;
            expr = Expr::FieldAccess { object: Box::new(expr), field, span };
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, CompileError> {
        let span = self.current_span();

        match self.current_kind().clone() {
            TokenKind::IntLiteral(v) => {
                let v = v;
                self.advance();
                Ok(Expr::IntLit(v, span))
            }
            TokenKind::FloatLiteral(v) => {
                let v = v;
                self.advance();
                Ok(Expr::FloatLit(v, span))
            }
            TokenKind::StringLiteral(s) => {
                let s = s.clone();
                self.advance();
                Ok(Expr::StringLit(s, span))
            }
            TokenKind::BoolLiteral(b) => {
                let b = b;
                self.advance();
                Ok(Expr::BoolLit(b, span))
            }
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();

                // Check for function call
                if self.check(TokenKind::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.check(TokenKind::RParen) {
                        if !args.is_empty() { self.expect(TokenKind::Comma)?; }
                        args.push(self.parse_expr()?);
                    }
                    self.expect(TokenKind::RParen)?;
                    Ok(Expr::Call { name, args, span })
                } else {
                    Ok(Expr::Ident(name, span))
                }
            }
            TokenKind::AtLocation => { self.advance(); self.parse_builtin_call(Builtin::Location, span) }
            TokenKind::AtProximity => { self.advance(); self.parse_builtin_call(Builtin::Proximity, span) }
            TokenKind::AtPresence => { self.advance(); self.parse_builtin_call(Builtin::Presence, span) }
            TokenKind::AtSensor => { self.advance(); self.parse_builtin_call(Builtin::Sensor, span) }
            TokenKind::AtBlockHeight => { self.advance(); self.parse_builtin_call(Builtin::BlockHeight, span) }
            TokenKind::AtBlockTime => { self.advance(); self.parse_builtin_call(Builtin::BlockTime, span) }
            TokenKind::AtCaller => { self.advance(); self.parse_builtin_call(Builtin::Caller, span) }
            TokenKind::AtBalance => { self.advance(); self.parse_builtin_call(Builtin::Balance, span) }
            TokenKind::AtStoreRead => { self.advance(); self.parse_builtin_call(Builtin::StoreRead, span) }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(expr)
            }
            _ => Err(CompileError::parse(
                format!("unexpected token: {:?}", self.current_kind()),
                &span,
            )),
        }
    }

    fn parse_builtin_call(&mut self, builtin: Builtin, span: Span) -> Result<Expr, CompileError> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        while !self.check(TokenKind::RParen) {
            if !args.is_empty() { self.expect(TokenKind::Comma)?; }
            args.push(self.parse_expr()?);
        }
        self.expect(TokenKind::RParen)?;
        Ok(Expr::BuiltinCall { builtin, args, span })
    }

    fn parse_type(&mut self) -> Result<Type, CompileError> {
        match self.current_kind() {
            TokenKind::TypeI32 => { self.advance(); Ok(Type::I32) }
            TokenKind::TypeI64 => { self.advance(); Ok(Type::I64) }
            TokenKind::TypeF32 => { self.advance(); Ok(Type::F32) }
            TokenKind::TypeF64 => { self.advance(); Ok(Type::F64) }
            TokenKind::TypeBool => { self.advance(); Ok(Type::Bool) }
            TokenKind::TypeString => { self.advance(); Ok(Type::String) }
            TokenKind::TypeBytes => { self.advance(); Ok(Type::Bytes) }
            TokenKind::TypeVoid => { self.advance(); Ok(Type::Void) }
            TokenKind::TypeAddress => { self.advance(); Ok(Type::Address) }
            _ => Err(CompileError::parse(
                format!("expected type, got {:?}", self.current_kind()),
                &self.current_span(),
            )),
        }
    }

    // ========================================================================
    // Token helpers
    // ========================================================================

    fn current(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn current_kind(&self) -> &TokenKind {
        &self.current().kind
    }

    fn current_span(&self) -> Span {
        self.current().span
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos + 1).map(|t| &t.kind)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(self.current_kind()) == std::mem::discriminant(&kind)
    }

    fn try_consume(&mut self, kind: TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<(), CompileError> {
        if self.check(kind.clone()) {
            self.advance();
            Ok(())
        } else {
            Err(CompileError::parse(
                format!("expected {:?}, got {:?}", kind, self.current_kind()),
                &self.current_span(),
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, CompileError> {
        match self.current_kind().clone() {
            TokenKind::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(CompileError::parse(
                format!("expected identifier, got {:?}", self.current_kind()),
                &self.current_span(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Contract {
        let tokens = Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens).parse_contract().unwrap()
    }

    #[test]
    fn test_empty_contract() {
        let c = parse("contract Empty {}");
        assert_eq!(c.name, "Empty");
        assert!(c.fields.is_empty());
        assert!(c.functions.is_empty());
    }

    #[test]
    fn test_contract_with_field() {
        let c = parse("contract Token { let supply: i64 = 1000; }");
        assert_eq!(c.fields.len(), 1);
        assert_eq!(c.fields[0].name, "supply");
        assert_eq!(c.fields[0].ty, Type::I64);
        assert!(c.fields[0].mutable);
    }

    #[test]
    fn test_const_field() {
        let c = parse("contract Token { const name: string = \"GRAT\"; }");
        assert_eq!(c.fields.len(), 1);
        assert!(!c.fields[0].mutable);
    }

    #[test]
    fn test_function_no_params() {
        let c = parse("contract Token { function getSupply(): i64 { return 1000; } }");
        assert_eq!(c.functions.len(), 1);
        assert_eq!(c.functions[0].name, "getSupply");
        assert_eq!(c.functions[0].return_type, Type::I64);
        assert!(c.functions[0].params.is_empty());
    }

    #[test]
    fn test_function_with_params() {
        let c = parse("contract Token { function transfer(to: address, amount: i64): bool { return true; } }");
        assert_eq!(c.functions[0].params.len(), 2);
        assert_eq!(c.functions[0].params[0].name, "to");
        assert_eq!(c.functions[0].params[0].ty, Type::Address);
    }

    #[test]
    fn test_if_else() {
        let c = parse("contract C { function f(): void { if (true) { return; } else { return; } } }");
        assert_eq!(c.functions[0].body.len(), 1);
        assert!(matches!(c.functions[0].body[0], Stmt::If { .. }));
    }

    #[test]
    fn test_while_loop() {
        let c = parse("contract C { function f(): void { while (true) { return; } } }");
        assert!(matches!(c.functions[0].body[0], Stmt::While { .. }));
    }

    #[test]
    fn test_builtin_call() {
        let c = parse("contract C { function f(): i32 { let p = @proximity(); return p; } }");
        let body = &c.functions[0].body;
        assert!(matches!(&body[0], Stmt::VarDecl { .. }));
    }

    #[test]
    fn test_emit() {
        let c = parse(r#"contract C { function f(): void { emit("transfer", "100 GRAT"); } }"#);
        assert!(matches!(c.functions[0].body[0], Stmt::Emit { .. }));
    }

    #[test]
    fn test_binary_ops() {
        let c = parse("contract C { function f(): i32 { let x = 1 + 2 * 3; return x; } }");
        // Should parse as 1 + (2 * 3) due to precedence
        if let Stmt::VarDecl { initializer, .. } = &c.functions[0].body[0] {
            assert!(matches!(initializer, Expr::BinOp { op: BinOp::Add, .. }));
        } else {
            panic!("expected VarDecl");
        }
    }

    #[test]
    fn test_comparison() {
        let c = parse("contract C { function f(): bool { return 1 < 2; } }");
        if let Stmt::Return { value: Some(expr), .. } = &c.functions[0].body[0] {
            assert!(matches!(expr, Expr::BinOp { op: BinOp::Lt, .. }));
        }
    }

    #[test]
    fn test_store_write() {
        let c = parse(r#"contract C { function f(): void { @store.write("key", "value"); } }"#);
        assert!(matches!(c.functions[0].body[0], Stmt::StoreWrite { .. }));
    }

    #[test]
    fn test_location_field_access() {
        let c = parse("contract C { function f(): f32 { let loc = @location(); return loc.lat; } }");
        if let Stmt::Return { value: Some(expr), .. } = &c.functions[0].body[1] {
            assert!(matches!(expr, Expr::FieldAccess { field, .. } if field == "lat"));
        }
    }

    #[test]
    fn test_full_contract() {
        let src = r#"
            contract LocationTrigger {
                let triggerLat: f32 = 0.0;
                let triggerLon: f32 = 0.0;
                let radius: f32 = 100.0;

                function configure(lat: f32, lon: f32, r: f32): void {
                    triggerLat = lat;
                    triggerLon = lon;
                    radius = r;
                }

                function check(): bool {
                    let loc = @location();
                    let dlat = loc.lat - triggerLat;
                    let dlon = loc.lon - triggerLon;
                    let dist = dlat * dlat + dlon * dlon;
                    if (dist < radius * radius) {
                        emit("triggered", "User entered zone");
                        return true;
                    }
                    return false;
                }
            }
        "#;
        let c = parse(src);
        assert_eq!(c.name, "LocationTrigger");
        assert_eq!(c.fields.len(), 3);
        assert_eq!(c.functions.len(), 2);
        assert_eq!(c.functions[0].name, "configure");
        assert_eq!(c.functions[1].name, "check");
    }
}
