//! Type checker for GratiaScript.
//!
//! Walks the AST and resolves types for every expression, checks that:
//! - Binary operations have compatible types (e.g., i32 + i32, not i32 + f32)
//! - Function call arguments match parameter types
//! - Return types match function signatures
//! - @builtin return types are correctly resolved
//! - Variables are declared before use
//!
//! The type checker runs after parsing and before code generation. It produces
//! a `TypedContract` that wraps the original AST with resolved type information,
//! which the code generator can use to select the correct WASM opcodes.

use std::collections::HashMap;

use crate::ast::*;
use crate::error::CompileError;
use crate::token::Span;

// ============================================================================
// Typed AST Output
// ============================================================================

/// A type-checked contract with resolved type information for every expression.
#[derive(Debug, Clone)]
pub struct TypedContract {
    /// The original contract AST (unchanged).
    pub contract: Contract,
    /// Resolved type for every expression, keyed by expression span.
    /// WHY: Using span as key avoids modifying the AST types. The code generator
    /// can look up the resolved type for any expression by its span.
    pub expr_types: HashMap<SpanKey, Type>,
    /// Resolved return type for each function, keyed by function name.
    pub function_return_types: HashMap<String, Type>,
}

/// A hashable key derived from a Span (start + end positions are unique).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanKey {
    pub start: usize,
    pub end: usize,
}

impl From<&Span> for SpanKey {
    fn from(span: &Span) -> Self {
        SpanKey {
            start: span.start,
            end: span.end,
        }
    }
}

// ============================================================================
// Symbol Table
// ============================================================================

/// Tracks variables in scope with their types.
#[derive(Debug, Clone)]
struct SymbolTable {
    /// Stack of scopes. Each scope is a map from variable name to its type.
    /// Index 0 is the global scope (contract fields), subsequent entries are
    /// nested block scopes.
    scopes: Vec<HashMap<String, SymbolInfo>>,
}

/// Information about a symbol (variable or parameter).
#[derive(Debug, Clone)]
struct SymbolInfo {
    ty: Type,
    mutable: bool,
}

impl SymbolTable {
    fn new() -> Self {
        SymbolTable {
            scopes: vec![HashMap::new()],
        }
    }

    /// Push a new scope (entering a block, function body, etc.).
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pop the current scope (leaving a block).
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Define a symbol in the current scope.
    fn define(&mut self, name: &str, ty: Type, mutable: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                SymbolInfo {
                    ty,
                    mutable,
                },
            );
        }
    }

    /// Look up a symbol by name, searching from innermost scope outward.
    fn lookup(&self, name: &str) -> Option<&SymbolInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.get(name) {
                return Some(info);
            }
        }
        None
    }
}

// ============================================================================
// Function Registry
// ============================================================================

/// Information about a function known to the type checker.
#[derive(Debug, Clone)]
struct FunctionInfo {
    params: Vec<(String, Type)>,
    return_type: Type,
}

// ============================================================================
// Type Checker
// ============================================================================

/// The GratiaScript type checker.
///
/// Walks the AST, resolves types for all expressions, and reports errors
/// when type rules are violated.
pub struct TypeChecker {
    /// Resolved expression types accumulated during checking.
    expr_types: HashMap<SpanKey, Type>,
    /// Symbol table for tracking variables in scope.
    symbols: SymbolTable,
    /// Known functions (contract functions).
    functions: HashMap<String, FunctionInfo>,
    /// The return type of the function currently being checked.
    current_return_type: Type,
}

impl TypeChecker {
    pub fn new() -> Self {
        TypeChecker {
            expr_types: HashMap::new(),
            symbols: SymbolTable::new(),
            functions: HashMap::new(),
            current_return_type: Type::Void,
        }
    }

    /// Type-check a contract and produce a `TypedContract` with resolved types.
    pub fn check(&mut self, contract: &Contract) -> Result<TypedContract, CompileError> {
        // Phase 1: Register contract fields in the global scope.
        for field in &contract.fields {
            self.symbols.define(&field.name, field.ty.clone(), field.mutable);
        }

        // Phase 2: Register all function signatures (forward declarations).
        // WHY: Functions can call each other in any order, so we need all
        // signatures available before checking any function body.
        for func in &contract.functions {
            let params: Vec<(String, Type)> = func
                .params
                .iter()
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect();
            self.functions.insert(
                func.name.clone(),
                FunctionInfo {
                    params,
                    return_type: func.return_type.clone(),
                },
            );
        }

        // Phase 3: Check field initializers.
        for field in &contract.fields {
            if let Some(init) = &field.initializer {
                let init_type = self.check_expr(init)?;
                if !types_compatible(&field.ty, &init_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "field '{}' declared as {} but initializer has type {}",
                            field.name, field.ty, init_type
                        ),
                        field.span.line,
                        field.span.col,
                    ));
                }
            }
        }

        // Phase 4: Check each function body.
        let mut function_return_types = HashMap::new();
        for func in &contract.functions {
            self.check_function(func)?;
            function_return_types.insert(func.name.clone(), func.return_type.clone());
        }

        Ok(TypedContract {
            contract: contract.clone(),
            expr_types: self.expr_types.clone(),
            function_return_types,
        })
    }

    // ========================================================================
    // Function checking
    // ========================================================================

    fn check_function(&mut self, func: &Function) -> Result<(), CompileError> {
        self.current_return_type = func.return_type.clone();

        // Push a new scope for the function body.
        self.symbols.push_scope();

        // Register parameters.
        for param in &func.params {
            self.symbols.define(&param.name, param.ty.clone(), false);
        }

        // Check the function body.
        for stmt in &func.body {
            self.check_stmt(stmt)?;
        }

        self.symbols.pop_scope();

        Ok(())
    }

    // ========================================================================
    // Statement checking
    // ========================================================================

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::VarDecl {
                name,
                ty,
                mutable,
                initializer,
                span,
            } => {
                let init_type = self.check_expr(initializer)?;

                let declared_type = if let Some(ty) = ty {
                    // Explicit type annotation — check compatibility.
                    if !types_compatible(ty, &init_type) {
                        return Err(CompileError::type_err(
                            format!(
                                "variable '{}' declared as {} but initializer has type {}",
                                name, ty, init_type
                            ),
                            span.line,
                            span.col,
                        ));
                    }
                    ty.clone()
                } else {
                    // No type annotation — infer from initializer.
                    init_type
                };

                self.symbols.define(name, declared_type, *mutable);
            }

            Stmt::Assign { target, value, span } => {
                // Check that the target exists and is mutable.
                let target_info = self.symbols.lookup(target).ok_or_else(|| {
                    CompileError::type_err(
                        format!("undefined variable: '{}'", target),
                        span.line,
                        span.col,
                    )
                })?;

                let target_type = target_info.ty.clone();
                let target_mutable = target_info.mutable;

                if !target_mutable {
                    return Err(CompileError::type_err(
                        format!("cannot assign to immutable variable '{}'", target),
                        span.line,
                        span.col,
                    ));
                }

                let value_type = self.check_expr(value)?;
                if !types_compatible(&target_type, &value_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "cannot assign {} to variable '{}' of type {}",
                            value_type, target, target_type
                        ),
                        span.line,
                        span.col,
                    ));
                }
            }

            Stmt::Return { value, span } => {
                let return_type = if let Some(expr) = value {
                    self.check_expr(expr)?
                } else {
                    Type::Void
                };

                if !types_compatible(&self.current_return_type, &return_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "return type mismatch: function expects {}, got {}",
                            self.current_return_type, return_type
                        ),
                        span.line,
                        span.col,
                    ));
                }
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
                span,
            } => {
                let cond_type = self.check_expr(condition)?;
                if !is_condition_type(&cond_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "if condition must be bool or i32, got {}",
                            cond_type
                        ),
                        span.line,
                        span.col,
                    ));
                }

                self.symbols.push_scope();
                for stmt in then_body {
                    self.check_stmt(stmt)?;
                }
                self.symbols.pop_scope();

                self.symbols.push_scope();
                for stmt in else_body {
                    self.check_stmt(stmt)?;
                }
                self.symbols.pop_scope();
            }

            Stmt::While {
                condition,
                body,
                span,
            } => {
                let cond_type = self.check_expr(condition)?;
                if !is_condition_type(&cond_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "while condition must be bool or i32, got {}",
                            cond_type
                        ),
                        span.line,
                        span.col,
                    ));
                }

                self.symbols.push_scope();
                for stmt in body {
                    self.check_stmt(stmt)?;
                }
                self.symbols.pop_scope();
            }

            Stmt::Expr(expr) => {
                self.check_expr(expr)?;
            }

            Stmt::Emit { topic, data, span } => {
                let topic_type = self.check_expr(topic)?;
                let data_type = self.check_expr(data)?;

                if topic_type != Type::String {
                    return Err(CompileError::type_err(
                        format!("emit topic must be string, got {}", topic_type),
                        span.line,
                        span.col,
                    ));
                }
                // Data can be any type — it gets serialized.
                let _ = data_type;
            }

            Stmt::StoreWrite { key, value, span } => {
                let key_type = self.check_expr(key)?;
                let _value_type = self.check_expr(value)?;

                if key_type != Type::String && key_type != Type::Bytes {
                    return Err(CompileError::type_err(
                        format!("storage key must be string or bytes, got {}", key_type),
                        span.line,
                        span.col,
                    ));
                }
            }
        }

        Ok(())
    }

    // ========================================================================
    // Expression checking
    // ========================================================================

    fn check_expr(&mut self, expr: &Expr) -> Result<Type, CompileError> {
        let resolved_type = match expr {
            Expr::IntLit(v, _span) => {
                // WHY: Small integers default to i32, large ones to i64.
                // This matches how the code generator emits i32.const vs i64.const.
                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    Type::I32
                } else {
                    Type::I64
                }
            }

            Expr::FloatLit(_, _span) => {
                // WHY: Float literals default to f32 in GratiaScript because
                // most contracts deal with GPS coordinates and sensor data
                // where f32 precision is sufficient and ARM f32 is faster.
                Type::F32
            }

            Expr::StringLit(_, _span) => Type::String,

            Expr::BoolLit(_, _span) => Type::Bool,

            Expr::Ident(name, span) => {
                let info = self.symbols.lookup(name).ok_or_else(|| {
                    CompileError::type_err(
                        format!("undefined variable: '{}'", name),
                        span.line,
                        span.col,
                    )
                })?;
                info.ty.clone()
            }

            Expr::BinOp {
                op,
                left,
                right,
                span,
            } => {
                let left_type = self.check_expr(left)?;
                let right_type = self.check_expr(right)?;

                self.check_binop(*op, &left_type, &right_type, span)?
            }

            Expr::UnaryOp {
                op,
                operand,
                span,
            } => {
                let operand_type = self.check_expr(operand)?;
                self.check_unaryop(*op, &operand_type, span)?
            }

            Expr::Call { name, args, span } => {
                self.check_call(name, args, span)?
            }

            Expr::BuiltinCall {
                builtin,
                args,
                span,
            } => {
                self.check_builtin_call(*builtin, args, span)?
            }

            Expr::FieldAccess {
                object,
                field,
                span,
            } => {
                let object_type = self.check_expr(object)?;
                self.check_field_access(&object_type, field, span)?
            }

            Expr::Cast {
                expr: inner,
                target_type,
                span,
            } => {
                let source_type = self.check_expr(inner)?;
                self.check_cast(&source_type, target_type, span)?;
                target_type.clone()
            }
        };

        // Record the resolved type for this expression.
        self.expr_types
            .insert(SpanKey::from(expr.span()), resolved_type.clone());

        Ok(resolved_type)
    }

    // ========================================================================
    // Binary operator type checking
    // ========================================================================

    fn check_binop(
        &self,
        op: BinOp,
        left: &Type,
        right: &Type,
        span: &Span,
    ) -> Result<Type, CompileError> {
        match op {
            // Arithmetic operators: both operands must be the same numeric type.
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                if !left.is_numeric() {
                    return Err(CompileError::type_err(
                        format!(
                            "operator {:?} requires numeric operands, got {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                if !types_compatible(left, right) {
                    return Err(CompileError::type_err(
                        format!(
                            "operator {:?}: incompatible types {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(left.clone())
            }

            // Modulo: integer operands only.
            BinOp::Mod => {
                if !left.is_integer() {
                    return Err(CompileError::type_err(
                        format!(
                            "operator % requires integer operands, got {} and {}",
                            left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                if !types_compatible(left, right) {
                    return Err(CompileError::type_err(
                        format!(
                            "operator %: incompatible types {} and {}",
                            left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(left.clone())
            }

            // Comparison operators: both operands must be the same numeric type.
            // Result is always Bool (represented as i32 in WASM).
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                if !types_compatible(left, right) {
                    return Err(CompileError::type_err(
                        format!(
                            "comparison {:?}: incompatible types {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::Bool)
            }

            // Logical operators: operands must be bool or i32 (which represents bool in WASM).
            BinOp::And | BinOp::Or => {
                if !is_condition_type(left) || !is_condition_type(right) {
                    return Err(CompileError::type_err(
                        format!(
                            "logical {:?}: operands must be bool or i32, got {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                // WHY: Result is i32 because that's how WASM represents booleans,
                // and AND/OR use i32.and/i32.or opcodes.
                Ok(Type::I32)
            }

            // Bitwise operators: integer operands only.
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                if !left.is_integer() {
                    return Err(CompileError::type_err(
                        format!(
                            "bitwise {:?}: requires integer operands, got {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                if !types_compatible(left, right) {
                    return Err(CompileError::type_err(
                        format!(
                            "bitwise {:?}: incompatible types {} and {}",
                            op, left, right
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(left.clone())
            }
        }
    }

    // ========================================================================
    // Unary operator type checking
    // ========================================================================

    fn check_unaryop(
        &self,
        op: UnaryOp,
        operand: &Type,
        span: &Span,
    ) -> Result<Type, CompileError> {
        match op {
            UnaryOp::Neg => {
                if !operand.is_numeric() {
                    return Err(CompileError::type_err(
                        format!("unary '-' requires numeric operand, got {}", operand),
                        span.line,
                        span.col,
                    ));
                }
                Ok(operand.clone())
            }
            UnaryOp::Not => {
                if !is_condition_type(operand) {
                    return Err(CompileError::type_err(
                        format!("unary '!' requires bool or i32, got {}", operand),
                        span.line,
                        span.col,
                    ));
                }
                // WHY: !expr uses i32.eqz, which returns i32.
                Ok(Type::I32)
            }
        }
    }

    // ========================================================================
    // Function call type checking
    // ========================================================================

    fn check_call(
        &mut self,
        name: &str,
        args: &[Expr],
        span: &Span,
    ) -> Result<Type, CompileError> {
        let func_info = self.functions.get(name).ok_or_else(|| {
            CompileError::type_err(
                format!("undefined function: '{}'", name),
                span.line,
                span.col,
            )
        })?.clone();

        // Check argument count.
        if args.len() != func_info.params.len() {
            return Err(CompileError::type_err(
                format!(
                    "function '{}' expects {} arguments, got {}",
                    name,
                    func_info.params.len(),
                    args.len()
                ),
                span.line,
                span.col,
            ));
        }

        // Check each argument type.
        for (i, arg) in args.iter().enumerate() {
            let arg_type = self.check_expr(arg)?;
            let (param_name, param_type) = &func_info.params[i];
            if !types_compatible(param_type, &arg_type) {
                return Err(CompileError::type_err(
                    format!(
                        "argument '{}' of function '{}' expects {}, got {}",
                        param_name, name, param_type, arg_type
                    ),
                    arg.span().line,
                    arg.span().col,
                ));
            }
        }

        Ok(func_info.return_type.clone())
    }

    // ========================================================================
    // Builtin call type checking
    // ========================================================================

    fn check_builtin_call(
        &mut self,
        builtin: Builtin,
        args: &[Expr],
        span: &Span,
    ) -> Result<Type, CompileError> {
        match builtin {
            Builtin::Location => {
                // @location() -> Location {lat: f32, lon: f32}
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@location() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::Location)
            }

            Builtin::Proximity => {
                // @proximity() -> i32
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@proximity() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::I32)
            }

            Builtin::Presence => {
                // @presence() -> i32
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@presence() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::I32)
            }

            Builtin::Sensor => {
                // @sensor(type: i32) -> f64
                if args.len() != 1 {
                    return Err(CompileError::type_err(
                        "@sensor() takes exactly 1 argument (sensor type)".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                let arg_type = self.check_expr(&args[0])?;
                if !types_compatible(&Type::I32, &arg_type) {
                    return Err(CompileError::type_err(
                        format!(
                            "@sensor() argument must be i32, got {}",
                            arg_type
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::F64)
            }

            Builtin::BlockHeight => {
                // @blockHeight() -> i64
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@blockHeight() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::I64)
            }

            Builtin::BlockTime => {
                // @blockTime() -> i64
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@blockTime() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::I64)
            }

            Builtin::Caller => {
                // @caller() -> Address
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@caller() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::Address)
            }

            Builtin::Balance => {
                // @balance() -> i64
                if !args.is_empty() {
                    return Err(CompileError::type_err(
                        "@balance() takes no arguments".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::I64)
            }

            Builtin::StoreRead => {
                // @store.read(key: string) -> Bytes
                if args.len() != 1 {
                    return Err(CompileError::type_err(
                        "@store.read() takes exactly 1 argument".to_string(),
                        span.line,
                        span.col,
                    ));
                }
                let arg_type = self.check_expr(&args[0])?;
                if arg_type != Type::String && arg_type != Type::Bytes {
                    return Err(CompileError::type_err(
                        format!(
                            "@store.read() key must be string or bytes, got {}",
                            arg_type
                        ),
                        span.line,
                        span.col,
                    ));
                }
                Ok(Type::Bytes)
            }
        }
    }

    // ========================================================================
    // Field access type checking
    // ========================================================================

    fn check_field_access(
        &self,
        object_type: &Type,
        field: &str,
        span: &Span,
    ) -> Result<Type, CompileError> {
        match object_type {
            Type::Location => {
                // Location has fields: lat (f32) and lon (f32).
                match field {
                    "lat" | "lon" => Ok(Type::F32),
                    _ => Err(CompileError::type_err(
                        format!("Location has no field '{}'", field),
                        span.line,
                        span.col,
                    )),
                }
            }
            _ => Err(CompileError::type_err(
                format!("type {} has no fields", object_type),
                span.line,
                span.col,
            )),
        }
    }

    // ========================================================================
    // Cast type checking
    // ========================================================================

    fn check_cast(
        &self,
        source: &Type,
        target: &Type,
        span: &Span,
    ) -> Result<(), CompileError> {
        // Allow casts between numeric types.
        if source.is_numeric() && target.is_numeric() {
            return Ok(());
        }

        // Allow bool <-> i32 (both are i32 in WASM).
        if (*source == Type::Bool && *target == Type::I32)
            || (*source == Type::I32 && *target == Type::Bool)
        {
            return Ok(());
        }

        Err(CompileError::type_err(
            format!("cannot cast {} to {}", source, target),
            span.line,
            span.col,
        ))
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Type Compatibility
// ============================================================================

/// Check if two types are compatible for assignment and comparison.
///
/// WHY: Bool and I32 are compatible because booleans are represented as i32
/// in WASM (0 = false, 1 = true). This allows comparisons like
/// `peers >= minPeers` where peers is i32 and the result is bool.
fn types_compatible(expected: &Type, actual: &Type) -> bool {
    if expected == actual {
        return true;
    }

    // Bool and I32 are interchangeable in WASM.
    if (*expected == Type::Bool && *actual == Type::I32)
        || (*expected == Type::I32 && *actual == Type::Bool)
    {
        return true;
    }

    // WHY: Implicit integer widening (i32 → i64) is always safe — no data loss.
    // This allows `let amount: i64 = 0;` without requiring a cast.
    // Similarly, f32 → f64 is safe (more precision, no loss).
    if *expected == Type::I64 && *actual == Type::I32 {
        return true;
    }
    if *expected == Type::F64 && *actual == Type::F32 {
        return true;
    }

    false
}

/// Check if a type can be used as a condition (if/while).
fn is_condition_type(ty: &Type) -> bool {
    matches!(ty, Type::Bool | Type::I32)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a span for testing.
    fn span(line: usize, col: usize) -> Span {
        Span::new(0, 0, line, col)
    }

    fn make_int_lit(v: i64) -> Expr {
        Expr::IntLit(v, span(1, 1))
    }

    fn make_float_lit(v: f64) -> Expr {
        Expr::FloatLit(v, span(1, 1))
    }

    fn make_bool_lit(v: bool) -> Expr {
        Expr::BoolLit(v, span(1, 1))
    }

    fn make_string_lit(s: &str) -> Expr {
        Expr::StringLit(s.to_string(), span(1, 1))
    }

    fn make_ident(name: &str) -> Expr {
        Expr::Ident(name.to_string(), span(1, 1))
    }

    // ========================================================================
    // Basic expression type resolution
    // ========================================================================

    #[test]
    fn test_int_literal_type() {
        let mut tc = TypeChecker::new();
        assert_eq!(tc.check_expr(&make_int_lit(42)).unwrap(), Type::I32);
    }

    #[test]
    fn test_large_int_literal_type() {
        let mut tc = TypeChecker::new();
        assert_eq!(
            tc.check_expr(&make_int_lit(3_000_000_000)).unwrap(),
            Type::I64
        );
    }

    #[test]
    fn test_float_literal_type() {
        let mut tc = TypeChecker::new();
        assert_eq!(tc.check_expr(&make_float_lit(3.14)).unwrap(), Type::F32);
    }

    #[test]
    fn test_bool_literal_type() {
        let mut tc = TypeChecker::new();
        assert_eq!(tc.check_expr(&make_bool_lit(true)).unwrap(), Type::Bool);
    }

    #[test]
    fn test_string_literal_type() {
        let mut tc = TypeChecker::new();
        assert_eq!(
            tc.check_expr(&make_string_lit("hello")).unwrap(),
            Type::String
        );
    }

    // ========================================================================
    // Variable resolution
    // ========================================================================

    #[test]
    fn test_variable_lookup() {
        let mut tc = TypeChecker::new();
        tc.symbols.define("x", Type::I32, true);
        assert_eq!(tc.check_expr(&make_ident("x")).unwrap(), Type::I32);
    }

    #[test]
    fn test_undefined_variable() {
        let mut tc = TypeChecker::new();
        let result = tc.check_expr(&make_ident("unknown"));
        assert!(result.is_err());
    }

    // ========================================================================
    // Binary operations
    // ========================================================================

    #[test]
    fn test_i32_addition() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::Add,
            left: Box::new(make_int_lit(1)),
            right: Box::new(make_int_lit(2)),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    #[test]
    fn test_f32_multiplication() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::Mul,
            left: Box::new(make_float_lit(1.0)),
            right: Box::new(make_float_lit(2.0)),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::F32);
    }

    #[test]
    fn test_incompatible_addition() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::Add,
            left: Box::new(make_int_lit(1)),
            right: Box::new(make_float_lit(2.0)),
            span: span(1, 1),
        };
        let result = tc.check_expr(&expr);
        assert!(result.is_err());
    }

    #[test]
    fn test_comparison_returns_bool() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::Lt,
            left: Box::new(make_int_lit(1)),
            right: Box::new(make_int_lit(2)),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::Bool);
    }

    #[test]
    fn test_modulo_requires_integer() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::Mod,
            left: Box::new(make_float_lit(1.0)),
            right: Box::new(make_float_lit(2.0)),
            span: span(1, 1),
        };
        let result = tc.check_expr(&expr);
        assert!(result.is_err());
    }

    #[test]
    fn test_logical_and() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BinOp {
            op: BinOp::And,
            left: Box::new(make_bool_lit(true)),
            right: Box::new(make_bool_lit(false)),
            span: span(1, 1),
        };
        // Logical AND returns i32.
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    // ========================================================================
    // Unary operations
    // ========================================================================

    #[test]
    fn test_negation() {
        let mut tc = TypeChecker::new();
        let expr = Expr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(make_int_lit(42)),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    #[test]
    fn test_not_operator() {
        let mut tc = TypeChecker::new();
        let expr = Expr::UnaryOp {
            op: UnaryOp::Not,
            operand: Box::new(make_bool_lit(true)),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    #[test]
    fn test_negation_on_string_fails() {
        let mut tc = TypeChecker::new();
        let expr = Expr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(make_string_lit("oops")),
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    // ========================================================================
    // Builtin calls
    // ========================================================================

    #[test]
    fn test_builtin_location_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Location,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::Location);
    }

    #[test]
    fn test_builtin_proximity_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Proximity,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    #[test]
    fn test_builtin_presence_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Presence,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I32);
    }

    #[test]
    fn test_builtin_sensor_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Sensor,
            args: vec![make_int_lit(0)],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::F64);
    }

    #[test]
    fn test_builtin_block_height_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::BlockHeight,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I64);
    }

    #[test]
    fn test_builtin_block_time_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::BlockTime,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I64);
    }

    #[test]
    fn test_builtin_caller_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Caller,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::Address);
    }

    #[test]
    fn test_builtin_balance_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Balance,
            args: vec![],
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::I64);
    }

    #[test]
    fn test_builtin_wrong_arg_count() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Proximity,
            args: vec![make_int_lit(1)],
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    #[test]
    fn test_builtin_sensor_wrong_arg_type() {
        let mut tc = TypeChecker::new();
        let expr = Expr::BuiltinCall {
            builtin: Builtin::Sensor,
            args: vec![make_float_lit(1.0)],
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    // ========================================================================
    // Field access
    // ========================================================================

    #[test]
    fn test_location_lat_field() {
        let mut tc = TypeChecker::new();
        let expr = Expr::FieldAccess {
            object: Box::new(Expr::BuiltinCall {
                builtin: Builtin::Location,
                args: vec![],
                span: span(1, 1),
            }),
            field: "lat".to_string(),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::F32);
    }

    #[test]
    fn test_location_lon_field() {
        let mut tc = TypeChecker::new();
        let expr = Expr::FieldAccess {
            object: Box::new(Expr::BuiltinCall {
                builtin: Builtin::Location,
                args: vec![],
                span: span(1, 1),
            }),
            field: "lon".to_string(),
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::F32);
    }

    #[test]
    fn test_location_invalid_field() {
        let mut tc = TypeChecker::new();
        let expr = Expr::FieldAccess {
            object: Box::new(Expr::BuiltinCall {
                builtin: Builtin::Location,
                args: vec![],
                span: span(1, 1),
            }),
            field: "alt".to_string(),
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    #[test]
    fn test_field_access_on_non_struct() {
        let mut tc = TypeChecker::new();
        let expr = Expr::FieldAccess {
            object: Box::new(make_int_lit(42)),
            field: "foo".to_string(),
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    // ========================================================================
    // Cast checking
    // ========================================================================

    #[test]
    fn test_cast_i32_to_f32() {
        let mut tc = TypeChecker::new();
        let expr = Expr::Cast {
            expr: Box::new(make_int_lit(42)),
            target_type: Type::F32,
            span: span(1, 1),
        };
        assert_eq!(tc.check_expr(&expr).unwrap(), Type::F32);
    }

    #[test]
    fn test_cast_string_to_i32_fails() {
        let mut tc = TypeChecker::new();
        let expr = Expr::Cast {
            expr: Box::new(make_string_lit("hello")),
            target_type: Type::I32,
            span: span(1, 1),
        };
        assert!(tc.check_expr(&expr).is_err());
    }

    // ========================================================================
    // Full contract type checking
    // ========================================================================

    #[test]
    fn test_check_simple_contract() {
        let contract = Contract {
            name: "Test".to_string(),
            fields: vec![Field {
                name: "count".to_string(),
                ty: Type::I32,
                mutable: true,
                initializer: Some(make_int_lit(0)),
                span: span(1, 1),
            }],
            functions: vec![Function {
                name: "getCount".to_string(),
                params: vec![],
                return_type: Type::I32,
                body: vec![Stmt::Return {
                    value: Some(make_ident("count")),
                    span: span(3, 5),
                }],
                span: span(2, 1),
            }],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        let typed = tc.check(&contract).unwrap();

        assert_eq!(typed.contract.name, "Test");
        assert!(typed.function_return_types.contains_key("getCount"));
        assert_eq!(typed.function_return_types["getCount"], Type::I32);
    }

    #[test]
    fn test_check_return_type_mismatch() {
        let contract = Contract {
            name: "Bad".to_string(),
            fields: vec![],
            functions: vec![Function {
                name: "broken".to_string(),
                params: vec![],
                return_type: Type::I32,
                body: vec![Stmt::Return {
                    value: Some(make_string_lit("wrong type")),
                    span: span(2, 5),
                }],
                span: span(1, 1),
            }],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        assert!(tc.check(&contract).is_err());
    }

    #[test]
    fn test_check_function_call_type() {
        let contract = Contract {
            name: "FuncCall".to_string(),
            fields: vec![],
            functions: vec![
                Function {
                    name: "helper".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: Type::I32,
                        span: span(1, 20),
                    }],
                    return_type: Type::I32,
                    body: vec![Stmt::Return {
                        value: Some(make_ident("x")),
                        span: span(2, 5),
                    }],
                    span: span(1, 1),
                },
                Function {
                    name: "caller".to_string(),
                    params: vec![],
                    return_type: Type::I32,
                    body: vec![Stmt::Return {
                        value: Some(Expr::Call {
                            name: "helper".to_string(),
                            args: vec![make_int_lit(42)],
                            span: span(5, 5),
                        }),
                        span: span(5, 1),
                    }],
                    span: span(4, 1),
                },
            ],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        let typed = tc.check(&contract).unwrap();
        assert_eq!(typed.function_return_types["caller"], Type::I32);
    }

    #[test]
    fn test_check_function_call_wrong_arg_type() {
        let contract = Contract {
            name: "BadCall".to_string(),
            fields: vec![],
            functions: vec![
                Function {
                    name: "helper".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: Type::I32,
                        span: span(1, 20),
                    }],
                    return_type: Type::I32,
                    body: vec![Stmt::Return {
                        value: Some(make_ident("x")),
                        span: span(2, 5),
                    }],
                    span: span(1, 1),
                },
                Function {
                    name: "caller".to_string(),
                    params: vec![],
                    return_type: Type::I32,
                    body: vec![Stmt::Return {
                        value: Some(Expr::Call {
                            name: "helper".to_string(),
                            args: vec![make_float_lit(3.14)], // Wrong type!
                            span: span(5, 5),
                        }),
                        span: span(5, 1),
                    }],
                    span: span(4, 1),
                },
            ],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        assert!(tc.check(&contract).is_err());
    }

    #[test]
    fn test_check_assign_to_immutable() {
        let contract = Contract {
            name: "Immutable".to_string(),
            fields: vec![Field {
                name: "x".to_string(),
                ty: Type::I32,
                mutable: false, // const
                initializer: Some(make_int_lit(42)),
                span: span(1, 1),
            }],
            functions: vec![Function {
                name: "tryAssign".to_string(),
                params: vec![],
                return_type: Type::Void,
                body: vec![Stmt::Assign {
                    target: "x".to_string(),
                    value: make_int_lit(99),
                    span: span(3, 5),
                }],
                span: span(2, 1),
            }],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        assert!(tc.check(&contract).is_err());
    }

    #[test]
    fn test_check_field_initializer_type_mismatch() {
        let contract = Contract {
            name: "BadInit".to_string(),
            fields: vec![Field {
                name: "x".to_string(),
                ty: Type::I32,
                mutable: true,
                initializer: Some(make_float_lit(3.14)),
                span: span(1, 1),
            }],
            functions: vec![],
            span: span(1, 1),
        };

        let mut tc = TypeChecker::new();
        assert!(tc.check(&contract).is_err());
    }

    // ========================================================================
    // Type compatibility
    // ========================================================================

    #[test]
    fn test_bool_i32_compatible() {
        assert!(types_compatible(&Type::Bool, &Type::I32));
        assert!(types_compatible(&Type::I32, &Type::Bool));
    }

    #[test]
    fn test_same_type_compatible() {
        assert!(types_compatible(&Type::I32, &Type::I32));
        assert!(types_compatible(&Type::F64, &Type::F64));
        assert!(types_compatible(&Type::String, &Type::String));
    }

    #[test]
    fn test_different_types_incompatible() {
        assert!(!types_compatible(&Type::I32, &Type::F32));
        assert!(!types_compatible(&Type::String, &Type::I32));
        assert!(!types_compatible(&Type::I64, &Type::F64));
    }

    // ========================================================================
    // Symbol table
    // ========================================================================

    #[test]
    fn test_symbol_table_scoping() {
        let mut table = SymbolTable::new();
        table.define("x", Type::I32, true);

        assert!(table.lookup("x").is_some());

        table.push_scope();
        // x is still visible from outer scope.
        assert!(table.lookup("x").is_some());

        // Define y in inner scope.
        table.define("y", Type::F32, false);
        assert!(table.lookup("y").is_some());

        table.pop_scope();
        // y is no longer visible.
        assert!(table.lookup("y").is_none());
        // x is still visible.
        assert!(table.lookup("x").is_some());
    }

    #[test]
    fn test_symbol_table_shadowing() {
        let mut table = SymbolTable::new();
        table.define("x", Type::I32, true);

        table.push_scope();
        table.define("x", Type::F32, false);

        // Inner x shadows outer x.
        let info = table.lookup("x").unwrap();
        assert_eq!(info.ty, Type::F32);
        assert!(!info.mutable);

        table.pop_scope();

        // Original x is back.
        let info = table.lookup("x").unwrap();
        assert_eq!(info.ty, Type::I32);
        assert!(info.mutable);
    }
}
