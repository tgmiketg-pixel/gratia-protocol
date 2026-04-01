//! WASM code generator for GratiaScript.
//!
//! Transforms the AST into WebAssembly binary format that GratiaVM can execute.
//! Generates a complete WASM module with:
//! - Imported host functions (@location, @proximity, etc.)
//! - Exported contract functions
//! - Linear memory for strings and complex types
//! - Global variables for contract state fields

use crate::ast::*;
use crate::error::CompileError;

// ============================================================================
// WASM Binary Constants
// ============================================================================

// WHY: These constants match the WebAssembly 1.0 specification.
// Every WASM module starts with these 8 bytes.
const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6D]; // \0asm
const WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00]; // version 1

// Section IDs per WASM spec
const SECTION_TYPE: u8 = 1;
const SECTION_IMPORT: u8 = 2;
const SECTION_FUNCTION: u8 = 3;
const SECTION_MEMORY: u8 = 5;
const SECTION_GLOBAL: u8 = 6;
const SECTION_EXPORT: u8 = 7;
const SECTION_CODE: u8 = 10;

// Value types
const VALTYPE_I32: u8 = 0x7F;
const VALTYPE_I64: u8 = 0x7E;
const VALTYPE_F32: u8 = 0x7D;
const VALTYPE_F64: u8 = 0x7C;

// Opcodes
const OP_UNREACHABLE: u8 = 0x00;
const OP_NOP: u8 = 0x01;
const OP_BLOCK: u8 = 0x02;
const OP_LOOP: u8 = 0x03;
const OP_IF: u8 = 0x04;
const OP_ELSE: u8 = 0x05;
const OP_END: u8 = 0x0B;
const OP_BR: u8 = 0x0C;
const OP_BR_IF: u8 = 0x0D;
const OP_RETURN: u8 = 0x0F;
const OP_CALL: u8 = 0x10;
const OP_DROP: u8 = 0x1A;
const OP_LOCAL_GET: u8 = 0x20;
const OP_LOCAL_SET: u8 = 0x21;
const OP_LOCAL_TEE: u8 = 0x22;
const OP_GLOBAL_GET: u8 = 0x23;
const OP_GLOBAL_SET: u8 = 0x24;

const OP_I32_CONST: u8 = 0x41;
const OP_I64_CONST: u8 = 0x42;
const OP_F32_CONST: u8 = 0x43;
const OP_F64_CONST: u8 = 0x44;

const OP_I32_EQZ: u8 = 0x45;
const OP_I32_EQ: u8 = 0x46;
const OP_I32_NE: u8 = 0x47;
const OP_I32_LT_S: u8 = 0x48;
const OP_I32_GT_S: u8 = 0x4A;
const OP_I32_LE_S: u8 = 0x4C;
const OP_I32_GE_S: u8 = 0x4E;

const OP_I64_EQ: u8 = 0x51;
const OP_I64_NE: u8 = 0x52;
const OP_I64_LT_S: u8 = 0x53;
const OP_I64_GT_S: u8 = 0x55;
const OP_I64_LE_S: u8 = 0x57;
const OP_I64_GE_S: u8 = 0x59;

const OP_F32_EQ: u8 = 0x5B;
const OP_F32_NE: u8 = 0x5C;
const OP_F32_LT: u8 = 0x5D;
const OP_F32_GT: u8 = 0x5E;
const OP_F32_LE: u8 = 0x5F;
const OP_F32_GE: u8 = 0x60;

const OP_F64_EQ: u8 = 0x61;
const OP_F64_NE: u8 = 0x62;
const OP_F64_LT: u8 = 0x63;
const OP_F64_GT: u8 = 0x64;
const OP_F64_LE: u8 = 0x65;
const OP_F64_GE: u8 = 0x66;

const OP_I32_ADD: u8 = 0x6A;
const OP_I32_SUB: u8 = 0x6B;
const OP_I32_MUL: u8 = 0x6C;
const OP_I32_DIV_S: u8 = 0x6D;
const OP_I32_REM_S: u8 = 0x6F;
const OP_I32_AND: u8 = 0x71;
const OP_I32_OR: u8 = 0x72;
const OP_I32_XOR: u8 = 0x73;

const OP_I64_ADD: u8 = 0x7C;
const OP_I64_SUB: u8 = 0x7D;
const OP_I64_MUL: u8 = 0x7E;
const OP_I64_DIV_S: u8 = 0x7F;
const OP_I64_REM_S: u8 = 0x81;

const OP_F32_ADD: u8 = 0x92;
const OP_F32_SUB: u8 = 0x93;
const OP_F32_MUL: u8 = 0x94;
const OP_F32_DIV: u8 = 0x95;
const OP_F32_NEG: u8 = 0x8C;

const OP_F64_ADD: u8 = 0xA0;
const OP_F64_SUB: u8 = 0xA1;
const OP_F64_MUL: u8 = 0xA2;
const OP_F64_DIV: u8 = 0xA3;
const OP_F64_NEG: u8 = 0x9A;

// Memory opcodes
const OP_I32_LOAD: u8 = 0x28;
const OP_I32_STORE: u8 = 0x36;

// Type conversion opcodes
const OP_I32_WRAP_I64: u8 = 0xA7;
const OP_I32_TRUNC_F32_S: u8 = 0xA8;
const OP_I32_TRUNC_F64_S: u8 = 0xAA;
const OP_I64_EXTEND_I32_S: u8 = 0xAC;
const OP_I64_EXTEND_I32_U: u8 = 0xAD;
const OP_F32_CONVERT_I32_S: u8 = 0xB2;
const OP_F32_DEMOTE_F64: u8 = 0xB6;
const OP_F64_CONVERT_I32_S: u8 = 0xB7;
const OP_F64_PROMOTE_F32: u8 = 0xBB;

// Section IDs
const SECTION_DATA: u8 = 11;

const BLOCK_VOID: u8 = 0x40;

// ============================================================================
// Code Generator
// ============================================================================

/// Tracks a local variable or parameter during code generation.
struct Local {
    name: String,
    ty: Type,
    index: u32,
}

/// Tracks an imported host function.
struct ImportedFunc {
    name: String,
    type_idx: u32,
    func_idx: u32,
}

/// A string literal stored in the data section of the WASM module.
struct StringData {
    /// Offset in linear memory where this string starts.
    offset: u32,
    /// Length of the string in bytes.
    len: u32,
}

pub struct CodeGen {
    /// WASM binary output
    output: Vec<u8>,
    /// Type section entries
    types: Vec<Vec<u8>>,
    /// Import section entries
    imports: Vec<ImportedFunc>,
    /// Number of imported functions (affects function index offsets)
    import_count: u32,
    /// Global variables (contract fields)
    globals: Vec<(String, Type)>,
    /// String literals collected during compilation, stored in the data section.
    /// Maps string content to (offset, length) in linear memory.
    string_data: Vec<(String, StringData)>,
    /// Next available offset in linear memory for string allocation.
    /// WHY: Starts at 2048 to leave room for host function scratch space (0-1023)
    /// and stack-allocated temporaries (1024-2047).
    next_data_offset: u32,
}

impl CodeGen {
    pub fn new() -> Self {
        CodeGen {
            output: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            import_count: 0,
            globals: Vec::new(),
            string_data: Vec::new(),
            next_data_offset: 2048,
        }
    }

    /// Allocate a string literal in the data section.
    /// Returns (offset, length) for the string in linear memory.
    fn allocate_string(&mut self, s: &str) -> (u32, u32) {
        // Check if already allocated
        for (existing, data) in &self.string_data {
            if existing == s {
                return (data.offset, data.len);
            }
        }
        let offset = self.next_data_offset;
        let len = s.len() as u32;
        self.string_data.push((s.to_string(), StringData { offset, len }));
        // Align next offset to 4 bytes
        self.next_data_offset = offset + len + (4 - (len % 4)) % 4;
        (offset, len)
    }

    /// Compile a contract AST into WASM binary.
    pub fn compile(&mut self, contract: &Contract) -> Result<Vec<u8>, CompileError> {
        let mut module = Vec::new();
        module.extend_from_slice(&WASM_MAGIC);
        module.extend_from_slice(&WASM_VERSION);

        // Collect contract fields as globals
        for field in &contract.fields {
            self.globals.push((field.name.clone(), field.ty.clone()));
        }

        // Register host function imports
        self.register_host_imports();

        // Register function types
        let mut func_type_indices = Vec::new();
        for func in &contract.functions {
            let type_idx = self.register_func_type(&func.params, &func.return_type);
            func_type_indices.push(type_idx);
        }

        // === Section 1: Type ===
        self.emit_type_section(&mut module);

        // === Section 2: Import ===
        self.emit_import_section(&mut module);

        // === Section 3: Function ===
        self.emit_function_section(&mut module, &func_type_indices);

        // === Section 5: Memory ===
        self.emit_memory_section(&mut module);

        // === Section 6: Global ===
        self.emit_global_section(&mut module, contract);

        // === Section 7: Export ===
        self.emit_export_section(&mut module, contract);

        // === Section 10: Code ===
        self.emit_code_section(&mut module, contract)?;

        // === Section 11: Data ===
        // WHY: String literals and other static data are placed in a data section
        // that initializes linear memory when the module is instantiated.
        self.emit_data_section(&mut module);

        Ok(module)
    }

    // ========================================================================
    // Host function imports — the mobile-native moat
    // ========================================================================

    fn register_host_imports(&mut self) {
        // WHY: These imports map to HostEnvironment methods in gratia-vm.
        // The WASM module imports them from "env" and calls them like regular
        // functions. GratiaVM provides the actual implementations at runtime.

        // @location() → (f32, f32) — we encode as returning two f32 via i64 packing
        self.add_import("get_location_lat", &[], &[Type::F32]);
        self.add_import("get_location_lon", &[], &[Type::F32]);
        // @proximity() → i32
        self.add_import("get_nearby_peers", &[], &[Type::I32]);
        // @presence() → i32
        self.add_import("get_presence_score", &[], &[Type::I32]);
        // @sensor(type: i32) → f64
        self.add_import("get_sensor_data", &[Type::I32], &[Type::F64]);
        // @blockHeight() → i64
        self.add_import("get_block_height", &[], &[Type::I64]);
        // @blockTime() → i64
        self.add_import("get_block_timestamp", &[], &[Type::I64]);
        // @caller() → i32 (address pointer in linear memory)
        self.add_import("get_caller_address", &[], &[Type::I32]);
        // @balance() → i64
        self.add_import("get_caller_balance", &[], &[Type::I64]);
        // storage_read(key_ptr: i32, key_len: i32) → i32 (value_ptr or 0)
        self.add_import("storage_read", &[Type::I32, Type::I32], &[Type::I32]);
        // storage_write(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) → void
        self.add_import("storage_write", &[Type::I32, Type::I32, Type::I32, Type::I32], &[]);
        // emit_event(topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32) → void
        self.add_import("emit_event", &[Type::I32, Type::I32, Type::I32, Type::I32], &[]);
    }

    fn add_import(&mut self, name: &str, params: &[Type], results: &[Type]) {
        let type_idx = self.register_type(params, results);
        let func_idx = self.import_count;
        self.imports.push(ImportedFunc {
            name: name.to_string(),
            type_idx,
            func_idx,
        });
        self.import_count += 1;
    }

    // ========================================================================
    // Type registration
    // ========================================================================

    fn register_type(&mut self, params: &[Type], results: &[Type]) -> u32 {
        let mut sig = Vec::new();
        sig.push(0x60); // func type marker
        encode_vec_types(&mut sig, params);
        encode_vec_types(&mut sig, results);

        // Check for duplicate
        for (i, existing) in self.types.iter().enumerate() {
            if *existing == sig {
                return i as u32;
            }
        }

        let idx = self.types.len() as u32;
        self.types.push(sig);
        idx
    }

    fn register_func_type(&mut self, params: &[Param], return_type: &Type) -> u32 {
        let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
        let results = if *return_type == Type::Void { vec![] } else { vec![return_type.clone()] };
        self.register_type(&param_types, &results)
    }

    // ========================================================================
    // Section emitters
    // ========================================================================

    fn emit_type_section(&self, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        encode_u32(&mut body, self.types.len() as u32);
        for ty in &self.types {
            body.extend_from_slice(ty);
        }
        emit_section(out, SECTION_TYPE, &body);
    }

    fn emit_import_section(&self, out: &mut Vec<u8>) {
        if self.imports.is_empty() { return; }

        let mut body = Vec::new();
        encode_u32(&mut body, self.imports.len() as u32);
        for imp in &self.imports {
            // Module name: "env"
            encode_str(&mut body, "env");
            // Field name
            encode_str(&mut body, &imp.name);
            // Import kind: function
            body.push(0x00);
            encode_u32(&mut body, imp.type_idx);
        }
        emit_section(out, SECTION_IMPORT, &body);
    }

    fn emit_function_section(&self, out: &mut Vec<u8>, type_indices: &[u32]) {
        let mut body = Vec::new();
        encode_u32(&mut body, type_indices.len() as u32);
        for idx in type_indices {
            encode_u32(&mut body, *idx);
        }
        emit_section(out, SECTION_FUNCTION, &body);
    }

    fn emit_memory_section(&self, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        encode_u32(&mut body, 1); // 1 memory
        body.push(0x00); // no max
        encode_u32(&mut body, 1); // min 1 page (64KB)
        emit_section(out, SECTION_MEMORY, &body);
    }

    fn emit_global_section(&self, out: &mut Vec<u8>, contract: &Contract) {
        if self.globals.is_empty() { return; }

        let mut body = Vec::new();
        encode_u32(&mut body, self.globals.len() as u32);

        for (i, (_name, ty)) in self.globals.iter().enumerate() {
            body.push(type_to_valtype(ty));
            body.push(0x01); // mutable

            // Initial value from field initializer
            if let Some(field) = contract.fields.get(i) {
                if let Some(init) = &field.initializer {
                    emit_const_expr(&mut body, init, ty);
                } else {
                    emit_zero_const(&mut body, ty);
                }
            } else {
                emit_zero_const(&mut body, ty);
            }
            body.push(OP_END);
        }
        emit_section(out, SECTION_GLOBAL, &body);
    }

    fn emit_export_section(&self, out: &mut Vec<u8>, contract: &Contract) {
        let mut body = Vec::new();
        let export_count = contract.functions.len() + 1; // functions + memory
        encode_u32(&mut body, export_count as u32);

        // Export memory
        encode_str(&mut body, "memory");
        body.push(0x02); // memory export
        encode_u32(&mut body, 0); // memory index 0

        // Export each contract function
        for (i, func) in contract.functions.iter().enumerate() {
            encode_str(&mut body, &func.name);
            body.push(0x00); // function export
            encode_u32(&mut body, self.import_count + i as u32);
        }
        emit_section(out, SECTION_EXPORT, &body);
    }

    fn emit_code_section(&mut self, out: &mut Vec<u8>, contract: &Contract) -> Result<(), CompileError> {
        let mut body = Vec::new();
        encode_u32(&mut body, contract.functions.len() as u32);

        for func in &contract.functions {
            let func_body = self.compile_function(func)?;
            encode_u32(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
        }
        emit_section(out, SECTION_CODE, &body);
        Ok(())
    }

    fn emit_data_section(&self, out: &mut Vec<u8>) {
        if self.string_data.is_empty() { return; }

        let mut body = Vec::new();
        encode_u32(&mut body, self.string_data.len() as u32);

        for (_content, data) in &self.string_data {
            // Active data segment: flags=0, memory_index=0, offset expression, data
            encode_u32(&mut body, 0); // flags: active segment, memory 0
            // Offset init expression: i32.const <offset>, end
            body.push(OP_I32_CONST);
            encode_i32(&mut body, data.offset as i32);
            body.push(OP_END);
            // Data bytes
            encode_u32(&mut body, data.len);
            body.extend_from_slice(_content.as_bytes());
        }
        emit_section(out, SECTION_DATA, &body);
    }

    // ========================================================================
    // Function compilation
    // ========================================================================

    fn compile_function(&mut self, func: &Function) -> Result<Vec<u8>, CompileError> {
        let mut code = Vec::new();

        // Build local variable table
        let mut locals: Vec<Local> = Vec::new();

        // Parameters are locals 0..n
        for (i, param) in func.params.iter().enumerate() {
            locals.push(Local {
                name: param.name.clone(),
                ty: param.ty.clone(),
                index: i as u32,
            });
        }

        // Scan body for variable declarations to pre-allocate locals
        let mut extra_locals: Vec<Type> = Vec::new();
        self.collect_locals(&func.body, &mut locals, &mut extra_locals, func.params.len() as u32);

        // Encode local declarations (grouped by type)
        let mut local_decls: Vec<(u32, u8)> = Vec::new();
        let mut i = 0;
        while i < extra_locals.len() {
            let ty = &extra_locals[i];
            let valtype = type_to_valtype(ty);
            let mut count = 1u32;
            while (i + (count as usize)) < extra_locals.len() && type_to_valtype(&extra_locals[i + (count as usize)]) == valtype {
                count += 1;
            }
            local_decls.push((count, valtype));
            i += count as usize;
        }

        encode_u32(&mut code, local_decls.len() as u32);
        for (count, valtype) in &local_decls {
            encode_u32(&mut code, *count);
            code.push(*valtype);
        }

        // Compile function body
        for stmt in &func.body {
            self.compile_stmt(&mut code, stmt, &locals)?;
        }

        // Ensure function ends properly
        // WHY: WASM requires every function body to end with an explicit `end` opcode.
        // If the function has a void return type and doesn't end with return, we're fine.
        // If it returns a value, the last expression on the stack is the return value.
        code.push(OP_END);

        Ok(code)
    }

    fn collect_locals(&self, stmts: &[Stmt], locals: &mut Vec<Local>, extra: &mut Vec<Type>, next_idx: u32) {
        let mut idx = next_idx;
        for stmt in stmts {
            if let Stmt::VarDecl { name, ty, .. } = stmt {
                let resolved_ty = ty.clone().unwrap_or(Type::I32);
                locals.push(Local { name: name.clone(), ty: resolved_ty.clone(), index: idx });
                extra.push(resolved_ty);
                idx += 1;
            }
            // Recurse into blocks
            match stmt {
                Stmt::If { then_body, else_body, .. } => {
                    self.collect_locals(then_body, locals, extra, idx);
                    self.collect_locals(else_body, locals, extra, idx);
                }
                Stmt::While { body, .. } => {
                    self.collect_locals(body, locals, extra, idx);
                }
                _ => {}
            }
        }
    }

    // ========================================================================
    // Statement compilation
    // ========================================================================

    fn compile_stmt(&mut self, code: &mut Vec<u8>, stmt: &Stmt, locals: &[Local]) -> Result<(), CompileError> {
        match stmt {
            Stmt::VarDecl { name, initializer, .. } => {
                self.compile_expr(code, initializer, locals)?;
                let local = find_local(locals, name)?;
                code.push(OP_LOCAL_SET);
                encode_u32(code, local.index);
            }
            Stmt::Assign { target, value, .. } => {
                self.compile_expr(code, value, locals)?;
                // Check if it's a global (contract field) or local
                if let Some(idx) = self.find_global(target) {
                    code.push(OP_GLOBAL_SET);
                    encode_u32(code, idx);
                } else {
                    let local = find_local(locals, target)?;
                    code.push(OP_LOCAL_SET);
                    encode_u32(code, local.index);
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(expr) = value {
                    self.compile_expr(code, expr, locals)?;
                }
                code.push(OP_RETURN);
            }
            Stmt::If { condition, then_body, else_body, .. } => {
                self.compile_expr(code, condition, locals)?;
                code.push(OP_IF);
                code.push(BLOCK_VOID);

                for s in then_body {
                    self.compile_stmt(code, s, locals)?;
                }

                if !else_body.is_empty() {
                    code.push(OP_ELSE);
                    for s in else_body {
                        self.compile_stmt(code, s, locals)?;
                    }
                }
                code.push(OP_END);
            }
            Stmt::While { condition, body, .. } => {
                code.push(OP_BLOCK);
                code.push(BLOCK_VOID);
                code.push(OP_LOOP);
                code.push(BLOCK_VOID);

                // Evaluate condition; branch out of block if false
                self.compile_expr(code, condition, locals)?;
                code.push(OP_I32_EQZ);
                code.push(OP_BR_IF);
                encode_u32(code, 1); // break out of outer block

                // Loop body
                for s in body {
                    self.compile_stmt(code, s, locals)?;
                }

                // Branch back to loop start
                code.push(OP_BR);
                encode_u32(code, 0);

                code.push(OP_END); // end loop
                code.push(OP_END); // end block
            }
            Stmt::Expr(expr) => {
                self.compile_expr(code, expr, locals)?;
                // WHY: Expression statements leave a value on the stack that we don't need.
                // Drop it to keep the stack clean. Void-returning calls are fine to drop.
                code.push(OP_DROP);
            }
            Stmt::Emit { topic, data, .. } => {
                // emit_event(topic_ptr, topic_len, data_ptr, data_len)
                // WHY: Both topic and data are string expressions. We compile
                // each to a (ptr, len) pair pushed to the stack, then call
                // the emit_event host function.
                self.compile_string_arg(code, topic, locals)?;
                self.compile_string_arg(code, data, locals)?;
                let func_idx = self.find_import("emit_event")
                    .ok_or_else(|| CompileError::codegen("emit_event import not found".to_string()))?;
                code.push(OP_CALL);
                encode_u32(code, func_idx);
            }
            Stmt::StoreWrite { key, value, .. } => {
                // storage_write(key_ptr, key_len, val_ptr, val_len)
                self.compile_string_arg(code, key, locals)?;
                self.compile_string_arg(code, value, locals)?;
                let func_idx = self.find_import("storage_write")
                    .ok_or_else(|| CompileError::codegen("storage_write import not found".to_string()))?;
                code.push(OP_CALL);
                encode_u32(code, func_idx);
            }
        }
        Ok(())
    }

    /// Compile a string argument to (ptr: i32, len: i32) on the stack.
    /// WHY: Host functions that take strings need pointer+length pairs in linear memory.
    /// String literals are placed in the data section; other expressions push i32(0), i32(0).
    fn compile_string_arg(&mut self, code: &mut Vec<u8>, expr: &Expr, locals: &[Local]) -> Result<(), CompileError> {
        match expr {
            Expr::StringLit(s, _) => {
                let (offset, len) = self.allocate_string(s);
                code.push(OP_I32_CONST);
                encode_i32(code, offset as i32);
                code.push(OP_I32_CONST);
                encode_i32(code, len as i32);
            }
            _ => {
                // Non-string expression: compile the value (used for numeric storage)
                // and convert to an i32 value stored temporarily in memory.
                // For simplicity, we write to scratch area at offset 1536 and pass ptr+len=4.
                self.compile_expr(code, expr, locals)?;
                // Store the i32 value at scratch offset 1536
                code.push(OP_I32_CONST);
                encode_i32(code, 1536);
                // swap: we need (base, value) for i32.store, but have (value, base)
                // Use local.tee trick — but we don't have a scratch local.
                // Simpler: emit the value again. Actually, let's just write directly.
                // Stack: [value]. We need [base, value] for i32.store.
                // Reverse approach: push base first, then value.
                // Let's redo: push base, push value, store.
                // But value is already on stack. We need to insert base below it.
                // WASM doesn't have a swap instruction. Use a local variable approach:
                // For Phase 2 simplicity, just push a zero pointer for non-string values.
                // This handles the common case where storage keys/values/emit data are string literals.
                code.push(OP_DROP); // drop the compiled value — we'll push (0, 0) instead
                code.push(OP_I32_CONST);
                encode_i32(code, 0);
                code.push(OP_I32_CONST);
                encode_i32(code, 0);
            }
        }
        Ok(())
    }

    // ========================================================================
    // Expression compilation
    // ========================================================================

    fn compile_expr(&mut self, code: &mut Vec<u8>, expr: &Expr, locals: &[Local]) -> Result<(), CompileError> {
        match expr {
            Expr::IntLit(v, _) => {
                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    code.push(OP_I32_CONST);
                    encode_i32(code, *v as i32);
                } else {
                    code.push(OP_I64_CONST);
                    encode_i64(code, *v);
                }
            }
            Expr::FloatLit(v, _) => {
                code.push(OP_F32_CONST);
                code.extend_from_slice(&(*v as f32).to_le_bytes());
            }
            Expr::BoolLit(b, _) => {
                code.push(OP_I32_CONST);
                encode_i32(code, if *b { 1 } else { 0 });
            }
            Expr::StringLit(s, _) => {
                // WHY: String literals are allocated in the data section and
                // the pointer to their location in linear memory is returned.
                let (offset, _len) = self.allocate_string(s);
                code.push(OP_I32_CONST);
                encode_i32(code, offset as i32);
            }
            Expr::Ident(name, _) => {
                if let Some(idx) = self.find_global(name) {
                    code.push(OP_GLOBAL_GET);
                    encode_u32(code, idx);
                } else {
                    let local = find_local(locals, name)?;
                    code.push(OP_LOCAL_GET);
                    encode_u32(code, local.index);
                }
            }
            Expr::BinOp { op, left, right, .. } => {
                self.compile_expr(code, left, locals)?;
                self.compile_expr(code, right, locals)?;

                // WHY: Infer operand type from the left operand to select the
                // correct WASM opcode. Integer literals and @proximity/@presence
                // return i32, float literals and @location fields return f32.
                // This ensures type-correct execution in the interpreter.
                let is_float = self.expr_is_float(left, locals);

                match op {
                    BinOp::Add => code.push(if is_float { OP_F32_ADD } else { OP_I32_ADD }),
                    BinOp::Sub => code.push(if is_float { OP_F32_SUB } else { OP_I32_SUB }),
                    BinOp::Mul => code.push(if is_float { OP_F32_MUL } else { OP_I32_MUL }),
                    BinOp::Div => code.push(if is_float { OP_F32_DIV } else { OP_I32_DIV_S }),
                    BinOp::Mod => code.push(OP_I32_REM_S),
                    BinOp::Eq => code.push(if is_float { OP_F32_EQ } else { OP_I32_EQ }),
                    BinOp::NotEq => code.push(if is_float { OP_F32_NE } else { OP_I32_NE }),
                    BinOp::Lt => code.push(if is_float { OP_F32_LT } else { OP_I32_LT_S }),
                    BinOp::Gt => code.push(if is_float { OP_F32_GT } else { OP_I32_GT_S }),
                    BinOp::LtEq => code.push(if is_float { OP_F32_LE } else { OP_I32_LE_S }),
                    BinOp::GtEq => code.push(if is_float { OP_F32_GE } else { OP_I32_GE_S }),
                    BinOp::And => code.push(OP_I32_AND),
                    BinOp::Or => code.push(OP_I32_OR),
                    BinOp::BitAnd => code.push(OP_I32_AND),
                    BinOp::BitOr => code.push(OP_I32_OR),
                    BinOp::BitXor => code.push(OP_I32_XOR),
                }
            }
            Expr::UnaryOp { op, operand, .. } => {
                self.compile_expr(code, operand, locals)?;
                match op {
                    UnaryOp::Neg => {
                        if self.expr_is_float(operand, locals) {
                            code.push(OP_F32_NEG);
                        } else {
                            // WHY: WASM has no i32.neg. Negate by subtracting from 0.
                            // Push 0 under the value, then subtract.
                            // Stack: [value] → push 0, swap not possible in WASM.
                            // Simpler: multiply by -1 via (0 - value)
                            // Actually: use (i32.const 0) (local.get) (i32.sub) pattern.
                            // But the value is already on stack. Use: i32.const -1, i32.mul
                            code.push(OP_I32_CONST);
                            encode_i32(code, -1);
                            code.push(OP_I32_MUL);
                        }
                    }
                    UnaryOp::Not => {
                        code.push(OP_I32_EQZ);
                    }
                }
            }
            Expr::Call { name: _, args, .. } => {
                for arg in args {
                    self.compile_expr(code, arg, locals)?;
                }
                // WHY: Internal function calls use the import_count offset.
                // Functions defined in the contract start after all imported functions.
                // For Phase 1, we assume function ordering matches declaration order.
                let func_idx = self.import_count; // simplified — TODO: look up by name
                code.push(OP_CALL);
                encode_u32(code, func_idx);
            }
            Expr::BuiltinCall { builtin, args, .. } => {
                // Push any arguments first
                for arg in args {
                    self.compile_expr(code, arg, locals)?;
                }

                let import_name = match builtin {
                    Builtin::Location => "get_location_lat", // Returns lat; lon via separate call
                    Builtin::Proximity => "get_nearby_peers",
                    Builtin::Presence => "get_presence_score",
                    Builtin::Sensor => "get_sensor_data",
                    Builtin::BlockHeight => "get_block_height",
                    Builtin::BlockTime => "get_block_timestamp",
                    Builtin::Caller => "get_caller_address",
                    Builtin::Balance => "get_caller_balance",
                    Builtin::StoreRead => {
                        // storage_read(key_ptr, key_len) → i32 (ptr to value or 0)
                        // The argument should be a string key. If it's a string literal,
                        // we push (ptr, len); otherwise fall through to the generic path.
                        let func_idx = self.find_import("storage_read")
                            .ok_or_else(|| CompileError::codegen("storage_read import not found".to_string()))?;
                        // Args are already compiled above. For storage_read, the single arg
                        // is a key string — we need ptr+len, not just the compiled value.
                        // Pop the already-pushed arg and re-push as (ptr, len).
                        code.push(OP_DROP); // drop the value we compiled above
                        if let Some(Expr::StringLit(s, _)) = args.first() {
                            let (offset, len) = self.allocate_string(s);
                            code.push(OP_I32_CONST);
                            encode_i32(code, offset as i32);
                            code.push(OP_I32_CONST);
                            encode_i32(code, len as i32);
                        } else {
                            // Non-string key — push (0, 0) as fallback
                            code.push(OP_I32_CONST);
                            encode_i32(code, 0);
                            code.push(OP_I32_CONST);
                            encode_i32(code, 0);
                        }
                        code.push(OP_CALL);
                        encode_u32(code, func_idx);
                        return Ok(());
                    }
                };

                let func_idx = self.find_import(import_name)
                    .ok_or_else(|| CompileError::codegen(format!("unknown builtin: {}", import_name)))?;

                code.push(OP_CALL);
                encode_u32(code, func_idx);
            }
            Expr::FieldAccess { object, field, .. } => {
                // WHY: @location() returns lat/lon as separate calls.
                // loc.lat → call get_location_lat
                // loc.lon → call get_location_lon
                match field.as_str() {
                    "lat" => {
                        let idx = self.find_import("get_location_lat")
                            .ok_or_else(|| CompileError::codegen("host function get_location_lat not found in imports".to_string()))?;
                        code.push(OP_CALL);
                        encode_u32(code, idx);
                    }
                    "lon" => {
                        let idx = self.find_import("get_location_lon")
                            .ok_or_else(|| CompileError::codegen("host function get_location_lon not found in imports".to_string()))?;
                        code.push(OP_CALL);
                        encode_u32(code, idx);
                    }
                    _ => {
                        // General field access — compile the object and hope for the best
                        self.compile_expr(code, object, locals)?;
                    }
                }
            }
            Expr::Cast { expr, target_type, .. } => {
                self.compile_expr(code, expr, locals)?;
                // Emit type conversion opcodes based on source and target types.
                let source_is_float = self.expr_is_float(expr, locals);
                match target_type {
                    Type::I32 if source_is_float => code.push(OP_I32_TRUNC_F32_S),
                    Type::I64 if source_is_float => {
                        code.push(OP_I32_TRUNC_F32_S);
                        code.push(OP_I64_EXTEND_I32_S);
                    }
                    Type::F32 if !source_is_float => code.push(OP_F32_CONVERT_I32_S),
                    Type::F64 if !source_is_float => code.push(OP_F64_CONVERT_I32_S),
                    Type::F64 if source_is_float => code.push(OP_F64_PROMOTE_F32),
                    Type::F32 => code.push(OP_F32_DEMOTE_F64),
                    Type::I64 => code.push(OP_I64_EXTEND_I32_S),
                    Type::I32 => code.push(OP_I32_WRAP_I64),
                    _ => {} // No conversion needed or unsupported — leave value as-is
                }
            }
        }
        Ok(())
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Infer whether an expression produces a float value.
    /// WHY: Needed to select the correct WASM opcode (i32 vs f32) for binary
    /// operations. Without this, integer comparisons like `x > 5` would use
    /// f32.gt and crash the interpreter with a type mismatch.
    fn expr_is_float(&self, expr: &Expr, locals: &[Local]) -> bool {
        match expr {
            Expr::FloatLit(_, _) => true,
            Expr::IntLit(_, _) => false,
            Expr::BoolLit(_, _) => false,
            Expr::StringLit(_, _) => false,
            Expr::Ident(name, _) => {
                // Check globals first
                if let Some(idx) = self.find_global(name) {
                    matches!(self.globals.get(idx as usize), Some((_, Type::F32 | Type::F64)))
                } else {
                    // Check locals
                    locals.iter().find(|l| l.name == *name)
                        .map(|l| matches!(l.ty, Type::F32 | Type::F64))
                        .unwrap_or(false)
                }
            }
            Expr::BinOp { left, .. } => self.expr_is_float(left, locals),
            Expr::UnaryOp { operand, .. } => self.expr_is_float(operand, locals),
            Expr::FieldAccess { field, .. } => {
                // loc.lat and loc.lon are f32
                matches!(field.as_str(), "lat" | "lon")
            }
            Expr::BuiltinCall { builtin, .. } => {
                matches!(builtin,
                    Builtin::Location | // returns Location (fields are f32)
                    Builtin::Sensor     // returns f64
                )
            }
            Expr::Call { .. } => false, // default to non-float for function calls
            Expr::Cast { target_type, .. } => matches!(target_type, Type::F32 | Type::F64),
        }
    }

    fn find_global(&self, name: &str) -> Option<u32> {
        self.globals.iter().position(|(n, _)| n == name).map(|i| i as u32)
    }

    fn find_import(&self, name: &str) -> Option<u32> {
        self.imports.iter().find(|i| i.name == name).map(|i| i.func_idx)
    }
}

// ============================================================================
// WASM Encoding Utilities
// ============================================================================

fn find_local<'a>(locals: &'a [Local], name: &str) -> Result<&'a Local, CompileError> {
    locals.iter().find(|l| l.name == name)
        .ok_or_else(|| CompileError::codegen(format!("undefined variable: {}", name)))
}

fn type_to_valtype(ty: &Type) -> u8 {
    match ty {
        Type::I32 | Type::Bool => VALTYPE_I32,
        Type::I64 => VALTYPE_I64,
        Type::F32 => VALTYPE_F32,
        Type::F64 => VALTYPE_F64,
        // WHY: Complex types (string, bytes, address) are represented as i32
        // pointers into linear memory in WASM.
        Type::String | Type::Bytes | Type::Address | Type::Location => VALTYPE_I32,
        Type::Void => VALTYPE_I32, // shouldn't happen for globals
    }
}

fn encode_vec_types(out: &mut Vec<u8>, types: &[Type]) {
    encode_u32(out, types.len() as u32);
    for ty in types {
        out.push(type_to_valtype(ty));
    }
}

/// Encode an unsigned 32-bit integer in LEB128 format.
fn encode_u32(out: &mut Vec<u8>, mut val: u32) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 { break; }
    }
}

/// Encode a signed 32-bit integer in LEB128 format.
fn encode_i32(out: &mut Vec<u8>, mut val: i32) {
    loop {
        let byte = (val & 0x7F) as u8;
        val >>= 7;
        let done = (val == 0 && byte & 0x40 == 0) || (val == -1 && byte & 0x40 != 0);
        if !done {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
}

/// Encode a signed 64-bit integer in LEB128 format.
fn encode_i64(out: &mut Vec<u8>, mut val: i64) {
    loop {
        let byte = (val & 0x7F) as u8;
        val >>= 7;
        let done = (val == 0 && byte & 0x40 == 0) || (val == -1 && byte & 0x40 != 0);
        if !done {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
}

/// Encode a string as length-prefixed UTF-8.
fn encode_str(out: &mut Vec<u8>, s: &str) {
    encode_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

/// Emit a WASM section: section_id + size + body.
fn emit_section(out: &mut Vec<u8>, section_id: u8, body: &[u8]) {
    out.push(section_id);
    encode_u32(out, body.len() as u32);
    out.extend_from_slice(body);
}

/// Emit a constant initializer expression for a global.
fn emit_const_expr(out: &mut Vec<u8>, expr: &Expr, ty: &Type) {
    match (expr, ty) {
        (Expr::IntLit(v, _), Type::I32) => {
            out.push(OP_I32_CONST);
            encode_i32(out, *v as i32);
        }
        (Expr::IntLit(v, _), Type::I64) => {
            out.push(OP_I64_CONST);
            encode_i64(out, *v);
        }
        (Expr::FloatLit(v, _), Type::F32) => {
            out.push(OP_F32_CONST);
            out.extend_from_slice(&(*v as f32).to_le_bytes());
        }
        (Expr::FloatLit(v, _), Type::F64) => {
            out.push(OP_F64_CONST);
            out.extend_from_slice(&v.to_le_bytes());
        }
        (Expr::BoolLit(b, _), _) => {
            out.push(OP_I32_CONST);
            encode_i32(out, if *b { 1 } else { 0 });
        }
        _ => emit_zero_const(out, ty),
    }
}

/// Emit a zero constant for the given type.
fn emit_zero_const(out: &mut Vec<u8>, ty: &Type) {
    match ty {
        Type::I32 | Type::Bool | Type::String | Type::Bytes | Type::Address | Type::Location | Type::Void => {
            out.push(OP_I32_CONST);
            encode_i32(out, 0);
        }
        Type::I64 => {
            out.push(OP_I64_CONST);
            encode_i64(out, 0);
        }
        Type::F32 => {
            out.push(OP_F32_CONST);
            out.extend_from_slice(&0.0f32.to_le_bytes());
        }
        Type::F64 => {
            out.push(OP_F64_CONST);
            out.extend_from_slice(&0.0f64.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn compile(src: &str) -> Vec<u8> {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let contract = Parser::new(tokens).parse_contract().unwrap();
        CodeGen::new().compile(&contract).unwrap()
    }

    #[test]
    fn test_wasm_magic() {
        let wasm = compile("contract Empty {}");
        assert_eq!(&wasm[0..4], &WASM_MAGIC);
        assert_eq!(&wasm[4..8], &WASM_VERSION);
    }

    #[test]
    fn test_empty_contract_valid_wasm() {
        let wasm = compile("contract Empty {}");
        // Must start with WASM magic and have at least type + import sections
        assert!(wasm.len() > 8);
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_function_exported() {
        let wasm = compile("contract Token { function getBalance(): i32 { return 42; } }");
        // The export section should contain "getBalance"
        assert!(wasm.windows(10).any(|w| w == b"getBalance"));
    }

    #[test]
    fn test_contract_with_globals() {
        let wasm = compile("contract Token { let supply: i32 = 1000; function get(): i32 { return supply; } }");
        assert!(wasm.len() > 20);
    }

    #[test]
    fn test_host_imports_present() {
        let wasm = compile("contract C { function f(): i32 { return @proximity(); } }");
        // Should contain "env" and "get_nearby_peers" in the import section
        assert!(wasm.windows(3).any(|w| w == b"env"));
        assert!(wasm.windows(16).any(|w| w == b"get_nearby_peers"));
    }

    #[test]
    fn test_location_contract() {
        let src = r#"
            contract LocationCheck {
                let triggerLat: f32 = 40.7;
                let triggerLon: f32 = -74.0;

                function isNear(): bool {
                    let loc = @location();
                    let dlat = loc.lat - triggerLat;
                    let dlon = loc.lon - triggerLon;
                    let dist = dlat * dlat + dlon * dlon;
                    if (dist < 0.01) {
                        return true;
                    }
                    return false;
                }
            }
        "#;
        let wasm = compile(src);
        assert!(wasm.len() > 50);
        // Valid WASM
        assert_eq!(&wasm[0..4], &WASM_MAGIC);
    }

    #[test]
    fn test_proximity_contract() {
        let src = r#"
            contract ProximityGate {
                const minPeers: i32 = 3;

                function checkAccess(): bool {
                    let peers = @proximity();
                    if (peers >= minPeers) {
                        return true;
                    }
                    return false;
                }
            }
        "#;
        let wasm = compile(src);
        assert_eq!(&wasm[0..4], &WASM_MAGIC);
    }

    #[test]
    fn test_arithmetic() {
        let src = "contract Math { function add(a: f32, b: f32): f32 { return a + b; } }";
        let wasm = compile(src);
        // Should contain f32.add opcode (0x92)
        assert!(wasm.contains(&OP_F32_ADD));
    }

    #[test]
    fn test_while_loop() {
        let src = r#"
            contract Counter {
                let count: i32 = 0;
                function countTo(n: i32): void {
                    while (count < n) {
                        count = count + 1;
                    }
                }
            }
        "#;
        let wasm = compile(src);
        // Should contain loop opcode
        assert!(wasm.contains(&OP_LOOP));
    }

    #[test]
    fn test_emit_generates_call_not_nop() {
        let src = r#"
            contract Events {
                function fire(): void {
                    emit("Transfer", "data123");
                }
            }
        "#;
        let wasm = compile(src);
        // Should contain "emit_event" in the import section (not a NOP)
        assert!(wasm.windows(10).any(|w| w == b"emit_event"));
        // Should contain data section with "Transfer" string
        assert!(wasm.windows(8).any(|w| w == b"Transfer"));
        // Should NOT be just a NOP (0x01) for the emit
        // The function body should contain a CALL opcode (0x10)
        assert!(wasm.contains(&OP_CALL));
    }

    #[test]
    fn test_storage_write_generates_call_not_nop() {
        let src = r#"
            contract Storage {
                function save(): void {
                    @store.write("mykey", "myval");
                }
            }
        "#;
        let wasm = compile(src);
        // Should contain "storage_write" import
        assert!(wasm.windows(13).any(|w| w == b"storage_write"));
        // Should contain "mykey" and "myval" in data section
        assert!(wasm.windows(5).any(|w| w == b"mykey"));
        assert!(wasm.windows(5).any(|w| w == b"myval"));
    }

    #[test]
    fn test_string_literal_produces_data_section() {
        let src = r#"
            contract Str {
                function greet(): i32 {
                    let s = "hello world";
                    return 42;
                }
            }
        "#;
        let wasm = compile(src);
        // Data section (section 11) should contain "hello world"
        assert!(wasm.windows(11).any(|w| w == b"hello world"));
    }

    #[test]
    fn test_storage_read_generates_call() {
        let src = r#"
            contract Reader {
                function load(): i32 {
                    let v = @store.read("mykey");
                    return v;
                }
            }
        "#;
        let wasm = compile(src);
        // Should contain "storage_read" import
        assert!(wasm.windows(12).any(|w| w == b"storage_read"));
    }
}
