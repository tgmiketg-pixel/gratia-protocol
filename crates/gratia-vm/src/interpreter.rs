//! Pure-Rust WASM interpreter for GratiaVM.
//!
//! This module provides `InterpreterRuntime`, a bytecode interpreter that can
//! execute the WASM binary output of the GratiaScript compiler without any
//! external WASM runtime dependency (no wasmer, no wasmtime). It implements
//! the `ContractRuntime` trait so it can be used as a drop-in replacement for
//! `MockRuntime` or the wasmer-backed `WasmerRuntime`.
//!
//! # Design
//!
//! The interpreter works in two phases:
//! 1. **Parse** — The WASM binary is decoded into an internal `WasmModule`
//!    representation (types, imports, functions, globals, exports, code).
//! 2. **Execute** — A stack-based interpreter walks the instruction stream,
//!    maintaining an operand stack (`Vec<Value>`), call frames, and gas metering.
//!
//! # Scope
//!
//! Only the WASM instructions emitted by the GratiaScript compiler are supported.
//! This is NOT a general-purpose WASM interpreter — it is purpose-built for the
//! Gratia smart contract pipeline.

use std::collections::HashMap;
use std::fmt;

use sha2::{Digest, Sha256};

use gratia_core::types::Address;

use crate::gas::{GasError, GasMeter};
use crate::host_functions::HostEnvironment;
use crate::runtime::{ContractRuntime, ContractValue, ExecutionOutcome, RuntimeError};
use crate::sandbox::{validate_bytecode, ContractPermissions, SandboxConfig};

// ============================================================================
// Value Types
// ============================================================================

/// Runtime value on the operand stack.
#[derive(Debug, Clone, Copy)]
enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Value {
    fn as_i32(&self) -> Result<i32, InterpError> {
        match self {
            Value::I32(v) => Ok(*v),
            _ => Err(InterpError::TypeMismatch {
                expected: "i32",
                got: self.type_name(),
            }),
        }
    }

    fn as_i64(&self) -> Result<i64, InterpError> {
        match self {
            Value::I64(v) => Ok(*v),
            _ => Err(InterpError::TypeMismatch {
                expected: "i64",
                got: self.type_name(),
            }),
        }
    }

    fn as_f32(&self) -> Result<f32, InterpError> {
        match self {
            Value::F32(v) => Ok(*v),
            _ => Err(InterpError::TypeMismatch {
                expected: "f32",
                got: self.type_name(),
            }),
        }
    }

    fn as_f64(&self) -> Result<f64, InterpError> {
        match self {
            Value::F64(v) => Ok(*v),
            _ => Err(InterpError::TypeMismatch {
                expected: "f64",
                got: self.type_name(),
            }),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::I32(_) => "i32",
            Value::I64(_) => "i64",
            Value::F32(_) => "f32",
            Value::F64(_) => "f64",
        }
    }

    fn default_for_valtype(vt: ValType) -> Value {
        match vt {
            ValType::I32 => Value::I32(0),
            ValType::I64 => Value::I64(0),
            ValType::F32 => Value::F32(0.0),
            ValType::F64 => Value::F64(0.0),
        }
    }

    fn to_contract_value(self) -> ContractValue {
        match self {
            Value::I32(v) => ContractValue::I32(v),
            Value::I64(v) => ContractValue::I64(v),
            Value::F32(v) => ContractValue::F32(v),
            Value::F64(v) => ContractValue::F64(v),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::I32(v) => write!(f, "i32({})", v),
            Value::I64(v) => write!(f, "i64({})", v),
            Value::F32(v) => write!(f, "f32({})", v),
            Value::F64(v) => write!(f, "f64({})", v),
        }
    }
}

// ============================================================================
// WASM Value Type
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValType {
    I32,
    I64,
    F32,
    F64,
}

impl ValType {
    fn from_byte(b: u8) -> Result<Self, InterpError> {
        match b {
            0x7F => Ok(ValType::I32),
            0x7E => Ok(ValType::I64),
            0x7D => Ok(ValType::F32),
            0x7C => Ok(ValType::F64),
            _ => Err(InterpError::InvalidBytecode(format!(
                "unknown value type: 0x{:02X}",
                b
            ))),
        }
    }
}

// ============================================================================
// WASM Module Internal Representation
// ============================================================================

/// A parsed WASM function type signature: (params) -> (results).
#[derive(Debug, Clone)]
struct FuncType {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// An imported function from the "env" module.
#[derive(Debug, Clone)]
struct Import {
    /// Module name (always "env" for GratiaScript contracts).
    #[allow(dead_code)]
    module: String,
    /// Function name (e.g. "get_location_lat").
    name: String,
    /// Type index into the module's type section.
    type_idx: u32,
}

/// A function defined in the WASM module (not imported).
#[derive(Debug, Clone)]
struct WasmFunction {
    /// Type index for this function's signature.
    type_idx: u32,
    /// Local variable types declared in the function body (excluding params).
    locals: Vec<ValType>,
    /// Raw bytecode for the function body (after local declarations, before final END).
    code: Vec<u8>,
}

/// A global variable.
#[derive(Debug, Clone)]
struct WasmGlobal {
    #[allow(dead_code)]
    valtype: ValType,
    #[allow(dead_code)]
    mutable: bool,
    /// Initial value (evaluated from const expression).
    init_value: Value,
}

/// An export entry.
#[derive(Debug, Clone)]
struct WasmExport {
    name: String,
    kind: ExportKind,
    index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportKind {
    Function,
    Memory,
    #[allow(dead_code)]
    Global,
}

/// A data segment to initialize linear memory.
#[derive(Debug, Clone)]
struct DataSegment {
    /// Offset in linear memory where this segment starts.
    offset: u32,
    /// The raw bytes to write at that offset.
    data: Vec<u8>,
}

/// The fully parsed WASM module.
#[derive(Debug, Clone)]
struct WasmModule {
    types: Vec<FuncType>,
    imports: Vec<Import>,
    functions: Vec<WasmFunction>,
    globals: Vec<WasmGlobal>,
    exports: Vec<WasmExport>,
    /// Number of imported functions (affects function index space).
    import_func_count: u32,
    /// Initial linear memory size in pages (64KB each).
    memory_min_pages: u32,
    /// Data segments for initializing linear memory.
    data_segments: Vec<DataSegment>,
}

// ============================================================================
// Interpreter Error
// ============================================================================

#[derive(Debug)]
enum InterpError {
    InvalidBytecode(String),
    StackUnderflow,
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
    },
    UndefinedFunction(u32),
    UndefinedGlobal(u32),
    UndefinedLocal(u32),
    DivisionByZero,
    /// Signals a return from the current function.
    FunctionReturn,
    /// Signals a branch to a label at the given depth.
    Branch(u32),
    Gas(GasError),
    HostError(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpError::InvalidBytecode(msg) => write!(f, "invalid bytecode: {}", msg),
            InterpError::StackUnderflow => write!(f, "stack underflow"),
            InterpError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {}, got {}", expected, got)
            }
            InterpError::UndefinedFunction(idx) => write!(f, "undefined function index: {}", idx),
            InterpError::UndefinedGlobal(idx) => write!(f, "undefined global index: {}", idx),
            InterpError::UndefinedLocal(idx) => write!(f, "undefined local index: {}", idx),
            InterpError::DivisionByZero => write!(f, "integer division by zero"),
            InterpError::FunctionReturn => write!(f, "function return (internal)"),
            InterpError::Branch(depth) => write!(f, "branch to depth {} (internal)", depth),
            InterpError::Gas(e) => write!(f, "gas error: {}", e),
            InterpError::HostError(msg) => write!(f, "host function error: {}", msg),
        }
    }
}

impl From<GasError> for InterpError {
    fn from(e: GasError) -> Self {
        InterpError::Gas(e)
    }
}

// ============================================================================
// Call Frame
// ============================================================================

/// A call frame on the interpreter's call stack.
struct CallFrame {
    /// Local variables (params + declared locals).
    locals: Vec<Value>,
    /// The function's return type (empty vec = void).
    result_types: Vec<ValType>,
    /// Stack depth when this frame was entered (for cleanup on return).
    stack_base: usize,
}

// ============================================================================
// WASM Binary Parser
// ============================================================================

/// Cursor-based reader for WASM binary data.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_byte(&mut self) -> Result<u8, InterpError> {
        if self.pos >= self.data.len() {
            return Err(InterpError::InvalidBytecode("unexpected end of data".into()));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], InterpError> {
        if self.pos + n > self.data.len() {
            return Err(InterpError::InvalidBytecode("unexpected end of data".into()));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn read_u32_leb128(&mut self) -> Result<u32, InterpError> {
        let mut result: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            result |= ((byte & 0x7F) as u32) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 35 {
                return Err(InterpError::InvalidBytecode("LEB128 too long".into()));
            }
        }
        Ok(result)
    }

    fn read_i32_leb128(&mut self) -> Result<i32, InterpError> {
        let mut result: i32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            result |= ((byte & 0x7F) as i32) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                // Sign extend
                if shift < 32 && (byte & 0x40) != 0 {
                    result |= !0i32 << shift;
                }
                break;
            }
            if shift > 35 {
                return Err(InterpError::InvalidBytecode("LEB128 too long".into()));
            }
        }
        Ok(result)
    }

    fn read_i64_leb128(&mut self) -> Result<i64, InterpError> {
        let mut result: i64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_byte()?;
            result |= ((byte & 0x7F) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && (byte & 0x40) != 0 {
                    result |= !0i64 << shift;
                }
                break;
            }
            if shift > 70 {
                return Err(InterpError::InvalidBytecode("LEB128 too long".into()));
            }
        }
        Ok(result)
    }

    fn read_f32(&mut self) -> Result<f32, InterpError> {
        let bytes = self.read_bytes(4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_f64(&mut self) -> Result<f64, InterpError> {
        let bytes = self.read_bytes(8)?;
        Ok(f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self) -> Result<String, InterpError> {
        let len = self.read_u32_leb128()? as usize;
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| InterpError::InvalidBytecode("invalid UTF-8 in string".into()))
    }
}

// ============================================================================
// Module Parser
// ============================================================================

fn parse_module(bytecode: &[u8]) -> Result<WasmModule, InterpError> {
    let mut reader = Reader::new(bytecode);

    // Validate magic and version
    let magic = reader.read_bytes(4)?;
    if magic != [0x00, 0x61, 0x73, 0x6D] {
        return Err(InterpError::InvalidBytecode("bad WASM magic".into()));
    }
    let version = reader.read_bytes(4)?;
    if version != [0x01, 0x00, 0x00, 0x00] {
        return Err(InterpError::InvalidBytecode("unsupported WASM version".into()));
    }

    let mut types = Vec::new();
    let mut imports = Vec::new();
    let mut func_type_indices: Vec<u32> = Vec::new();
    let mut globals = Vec::new();
    let mut exports = Vec::new();
    let mut code_bodies: Vec<(Vec<ValType>, Vec<u8>)> = Vec::new();
    let mut memory_min_pages: u32 = 0;
    let mut data_segments: Vec<DataSegment> = Vec::new();

    // Parse sections
    while reader.remaining() > 0 {
        let section_id = reader.read_byte()?;
        let section_size = reader.read_u32_leb128()? as usize;
        let section_end = reader.pos + section_size;

        match section_id {
            // Type section
            1 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let marker = reader.read_byte()?;
                    if marker != 0x60 {
                        return Err(InterpError::InvalidBytecode("expected func type marker 0x60".into()));
                    }
                    let param_count = reader.read_u32_leb128()?;
                    let mut params = Vec::new();
                    for _ in 0..param_count {
                        params.push(ValType::from_byte(reader.read_byte()?)?);
                    }
                    let result_count = reader.read_u32_leb128()?;
                    let mut results = Vec::new();
                    for _ in 0..result_count {
                        results.push(ValType::from_byte(reader.read_byte()?)?);
                    }
                    types.push(FuncType { params, results });
                }
            }
            // Import section
            2 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let module = reader.read_string()?;
                    let name = reader.read_string()?;
                    let kind = reader.read_byte()?;
                    if kind != 0x00 {
                        return Err(InterpError::InvalidBytecode(
                            format!("unsupported import kind: 0x{:02X} (only functions supported)", kind),
                        ));
                    }
                    let type_idx = reader.read_u32_leb128()?;
                    imports.push(Import {
                        module,
                        name,
                        type_idx,
                    });
                }
            }
            // Function section
            3 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    func_type_indices.push(reader.read_u32_leb128()?);
                }
            }
            // Memory section
            5 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let flags = reader.read_byte()?;
                    let min_pages = reader.read_u32_leb128()?;
                    memory_min_pages = min_pages;
                    if flags & 0x01 != 0 {
                        // Has max pages — read and discard
                        let _max_pages = reader.read_u32_leb128()?;
                    }
                }
            }
            // Global section
            6 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let valtype = ValType::from_byte(reader.read_byte()?)?;
                    let mutable = reader.read_byte()? == 0x01;
                    let init_value = parse_const_expr(&mut reader, valtype)?;
                    globals.push(WasmGlobal {
                        valtype,
                        mutable,
                        init_value,
                    });
                }
            }
            // Export section
            7 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let name = reader.read_string()?;
                    let kind_byte = reader.read_byte()?;
                    let kind = match kind_byte {
                        0x00 => ExportKind::Function,
                        0x02 => ExportKind::Memory,
                        0x03 => ExportKind::Global,
                        _ => {
                            // Skip unknown export kinds
                            reader.read_u32_leb128()?;
                            continue;
                        }
                    };
                    let index = reader.read_u32_leb128()?;
                    exports.push(WasmExport { name, kind, index });
                }
            }
            // Code section
            10 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let body_size = reader.read_u32_leb128()? as usize;
                    let body_start = reader.pos;
                    let body_end = body_start + body_size;

                    // Parse local declarations
                    let local_decl_count = reader.read_u32_leb128()?;
                    let mut locals = Vec::new();
                    for _ in 0..local_decl_count {
                        let count = reader.read_u32_leb128()?;
                        let vt = ValType::from_byte(reader.read_byte()?)?;
                        for _ in 0..count {
                            locals.push(vt);
                        }
                    }

                    // The rest until body_end is the instruction stream (including final END).
                    // WHY: We store the raw bytes and interpret them at execution time.
                    let code_len = body_end - reader.pos;
                    let code = reader.read_bytes(code_len)?.to_vec();

                    code_bodies.push((locals, code));
                }
            }
            // Data section — initializes linear memory with string literals etc.
            11 => {
                let count = reader.read_u32_leb128()?;
                for _ in 0..count {
                    let segment_flags = reader.read_u32_leb128()?;
                    let offset = if segment_flags == 0 {
                        // Active segment with memory index 0: has an offset expression
                        let offset_val = parse_const_expr(&mut reader, ValType::I32)?;
                        offset_val.as_i32().unwrap_or(0) as u32
                    } else {
                        // Passive or other — use offset 0
                        0
                    };
                    let data_len = reader.read_u32_leb128()? as usize;
                    let data = reader.read_bytes(data_len)?.to_vec();
                    data_segments.push(DataSegment { offset, data });
                }
            }
            // Unknown/unsupported section — skip
            _ => {
                reader.pos = section_end;
            }
        }

        // Ensure we consumed exactly the section
        if reader.pos != section_end {
            // WHY: Some sections may have trailing data we didn't parse.
            // Advance to section_end to stay in sync.
            reader.pos = section_end;
        }
    }

    // Build WasmFunction entries by pairing function type indices with code bodies.
    let import_func_count = imports.len() as u32;
    let mut functions = Vec::new();
    for (i, type_idx) in func_type_indices.iter().enumerate() {
        if i < code_bodies.len() {
            let (locals, code) = code_bodies[i].clone();
            functions.push(WasmFunction {
                type_idx: *type_idx,
                locals,
                code,
            });
        }
    }

    Ok(WasmModule {
        types,
        imports,
        functions,
        globals,
        exports,
        import_func_count,
        memory_min_pages,
        data_segments,
    })
}

/// Parse a constant initializer expression (for globals).
/// These are very restricted: just a const instruction followed by END.
fn parse_const_expr(reader: &mut Reader, valtype: ValType) -> Result<Value, InterpError> {
    let opcode = reader.read_byte()?;
    let value = match opcode {
        0x41 => {
            // i32.const
            let v = reader.read_i32_leb128()?;
            Value::I32(v)
        }
        0x42 => {
            // i64.const
            let v = reader.read_i64_leb128()?;
            Value::I64(v)
        }
        0x43 => {
            // f32.const
            let v = reader.read_f32()?;
            Value::F32(v)
        }
        0x44 => {
            // f64.const
            let v = reader.read_f64()?;
            Value::F64(v)
        }
        _ => {
            // If we can't parse the init expression, use a zero default.
            // WHY: Some global init expressions might use instructions we
            // don't fully handle; defaulting to zero is safe for Phase 1.
            return Ok(Value::default_for_valtype(valtype));
        }
    };

    // Consume the END opcode.
    let end = reader.read_byte()?;
    if end != 0x0B {
        return Err(InterpError::InvalidBytecode(
            "expected END after const expression".into(),
        ));
    }

    Ok(value)
}

// ============================================================================
// Interpreter Engine
// ============================================================================

/// Execute a function in the given module.
fn execute_function(
    module: &WasmModule,
    func_idx: u32,
    args: &[Value],
    globals: &mut Vec<Value>,
    memory: &mut Vec<u8>,
    gas_meter: &mut GasMeter,
    host_env: &mut HostEnvironment,
    permissions: &ContractPermissions,
    call_depth: u32,
) -> Result<Option<Value>, InterpError> {
    // WHY: Limit call depth to prevent stack overflow from recursive contracts.
    // 256 is generous but bounded.
    const MAX_CALL_DEPTH: u32 = 256;
    if call_depth > MAX_CALL_DEPTH {
        return Err(InterpError::HostError("call depth exceeded".into()));
    }

    // Check if it's an imported function (host call).
    if func_idx < module.import_func_count {
        return dispatch_host_call(module, func_idx, args, memory, gas_meter, host_env, permissions);
    }

    // Internal function
    let internal_idx = (func_idx - module.import_func_count) as usize;
    let func = module
        .functions
        .get(internal_idx)
        .ok_or(InterpError::UndefinedFunction(func_idx))?;
    let func_type = module
        .types
        .get(func.type_idx as usize)
        .ok_or(InterpError::UndefinedFunction(func_idx))?;

    // Build locals: params first, then declared locals initialized to zero.
    let mut locals = Vec::new();
    for (i, param_type) in func_type.params.iter().enumerate() {
        if i < args.len() {
            locals.push(args[i]);
        } else {
            locals.push(Value::default_for_valtype(*param_type));
        }
    }
    for local_type in &func.locals {
        locals.push(Value::default_for_valtype(*local_type));
    }

    // Create the operand stack for this call.
    let mut stack: Vec<Value> = Vec::new();

    // Execute the code.
    let mut code_reader = Reader::new(&func.code);
    let result = execute_block(
        module,
        &mut code_reader,
        &mut stack,
        &mut locals,
        globals,
        memory,
        gas_meter,
        host_env,
        permissions,
        call_depth,
        &func_type.results,
    );

    match result {
        Ok(()) | Err(InterpError::FunctionReturn) => {
            // Return the top-of-stack value if the function has a result type.
            if !func_type.results.is_empty() {
                if let Some(val) = stack.pop() {
                    Ok(Some(val))
                } else {
                    // WHY: If the stack is empty but a return type is expected,
                    // return a zero default rather than erroring. Some compiler
                    // code paths may leave the stack empty after a branch.
                    Ok(Some(Value::default_for_valtype(func_type.results[0])))
                }
            } else {
                Ok(None)
            }
        }
        Err(InterpError::Branch(_)) => {
            // Branch escaped function scope — treat as return.
            if !func_type.results.is_empty() {
                Ok(stack.pop().or(Some(Value::default_for_valtype(func_type.results[0]))))
            } else {
                Ok(None)
            }
        }
        Err(e) => Err(e),
    }
}

/// Execute a block of instructions. Returns Ok(()) on normal completion,
/// Err(Branch(depth)) for a branch, Err(FunctionReturn) for a return.
fn execute_block(
    module: &WasmModule,
    reader: &mut Reader,
    stack: &mut Vec<Value>,
    locals: &mut Vec<Value>,
    globals: &mut Vec<Value>,
    memory: &mut Vec<u8>,
    gas_meter: &mut GasMeter,
    host_env: &mut HostEnvironment,
    permissions: &ContractPermissions,
    call_depth: u32,
    _result_types: &[ValType],
) -> Result<(), InterpError> {
    while reader.remaining() > 0 {
        let opcode = reader.read_byte()?;

        // Charge gas for every instruction.
        // WHY: Per-instruction metering prevents unbounded execution.
        // Arithmetic costs 1 gas, divisions cost 5, branches cost 2.
        match opcode {
            0x6D | 0x6F | 0x7F | 0x81 | 0x95 | 0xA3 => {
                // Division and remainder opcodes — charge division cost.
                gas_meter.charge_division()?;
            }
            0x02 | 0x03 | 0x04 | 0x0C | 0x0D => {
                // Block, loop, if, br, br_if — charge branch cost.
                gas_meter.charge_branch()?;
            }
            0x0B | 0x01 | 0x1A => {
                // end, nop, drop — free (structural opcodes).
            }
            0x28..=0x2D | 0x36..=0x3A => {
                // Memory load/store — charge memory cost.
                gas_meter.charge_arithmetic()?;
            }
            0x3F | 0x40 => {
                // memory.size / memory.grow — charge branch cost.
                gas_meter.charge_branch()?;
            }
            _ => {
                // All other opcodes — charge arithmetic cost.
                gas_meter.charge_arithmetic()?;
            }
        }

        match opcode {
            // === Control Flow ===

            // unreachable
            0x00 => {
                return Err(InterpError::HostError("unreachable executed".into()));
            }

            // nop
            0x01 => {}

            // block
            0x02 => {
                let _block_type = reader.read_byte()?; // 0x40 = void
                let block_start = reader.pos;
                // Execute the block body
                match execute_block(
                    module, reader, stack, locals, globals, memory, gas_meter, host_env,
                    permissions, call_depth, &[],
                ) {
                    Ok(()) => {}
                    Err(InterpError::Branch(0)) => {
                        // Branch targets this block — break out of block.
                        // Skip to the matching END.
                    }
                    Err(InterpError::Branch(n)) => {
                        // Branch targets an outer block — propagate with decremented depth.
                        return Err(InterpError::Branch(n - 1));
                    }
                    Err(e) => return Err(e),
                }
            }

            // loop
            0x03 => {
                let _block_type = reader.read_byte()?;
                let loop_start = reader.pos;
                loop {
                    reader.pos = loop_start;
                    match execute_block(
                        module, reader, stack, locals, globals, memory, gas_meter, host_env,
                        permissions, call_depth, &[],
                    ) {
                        Ok(()) => break,
                        Err(InterpError::Branch(0)) => {
                            // Branch targets this loop — continue from the top.
                            continue;
                        }
                        Err(InterpError::Branch(n)) => {
                            return Err(InterpError::Branch(n - 1));
                        }
                        Err(e) => return Err(e),
                    }
                }
            }

            // if
            0x04 => {
                let _block_type = reader.read_byte()?;
                let condition = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;

                if condition != 0 {
                    // Execute then branch
                    match execute_block(
                        module, reader, stack, locals, globals, memory, gas_meter, host_env,
                        permissions, call_depth, &[],
                    ) {
                        Ok(()) => {}
                        Err(InterpError::Branch(0)) => {}
                        Err(InterpError::Branch(n)) => return Err(InterpError::Branch(n - 1)),
                        Err(e) => return Err(e),
                    }
                } else {
                    // Skip to else or end
                    skip_to_else_or_end(reader)?;
                    // If we stopped at else, execute the else branch
                    if reader.pos > 0 && reader.data[reader.pos - 1] == 0x05 {
                        match execute_block(
                            module, reader, stack, locals, globals, memory, gas_meter, host_env,
                            permissions, call_depth, &[],
                        ) {
                            Ok(()) => {}
                            Err(InterpError::Branch(0)) => {}
                            Err(InterpError::Branch(n)) => return Err(InterpError::Branch(n - 1)),
                            Err(e) => return Err(e),
                        }
                    }
                }
            }

            // else — when we reach else during then-branch execution,
            // skip to end of the if block.
            0x05 => {
                skip_to_end(reader)?;
                return Ok(());
            }

            // end
            0x0B => {
                return Ok(());
            }

            // br
            0x0C => {
                let depth = reader.read_u32_leb128()?;
                return Err(InterpError::Branch(depth));
            }

            // br_if
            0x0D => {
                let depth = reader.read_u32_leb128()?;
                let condition = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                if condition != 0 {
                    return Err(InterpError::Branch(depth));
                }
            }

            // return
            0x0F => {
                return Err(InterpError::FunctionReturn);
            }

            // call
            0x10 => {
                let func_idx = reader.read_u32_leb128()?;

                // Determine argument count from the function type.
                let type_idx = if func_idx < module.import_func_count {
                    module.imports[func_idx as usize].type_idx
                } else {
                    let internal = (func_idx - module.import_func_count) as usize;
                    module
                        .functions
                        .get(internal)
                        .ok_or(InterpError::UndefinedFunction(func_idx))?
                        .type_idx
                };
                let func_type = module
                    .types
                    .get(type_idx as usize)
                    .ok_or(InterpError::UndefinedFunction(func_idx))?;

                // Pop arguments from stack (they were pushed left-to-right,
                // so we need to reverse the order when popping).
                let param_count = func_type.params.len();
                if stack.len() < param_count {
                    return Err(InterpError::StackUnderflow);
                }
                let args: Vec<Value> = stack.split_off(stack.len() - param_count);

                let result = execute_function(
                    module,
                    func_idx,
                    &args,
                    globals,
                    memory,
                    gas_meter,
                    host_env,
                    permissions,
                    call_depth + 1,
                )?;

                if let Some(val) = result {
                    stack.push(val);
                }
            }

            // drop
            0x1A => {
                stack.pop().ok_or(InterpError::StackUnderflow)?;
            }

            // === Variable Access ===

            // local.get
            0x20 => {
                let idx = reader.read_u32_leb128()? as usize;
                let val = *locals
                    .get(idx)
                    .ok_or(InterpError::UndefinedLocal(idx as u32))?;
                stack.push(val);
            }

            // local.set
            0x21 => {
                let idx = reader.read_u32_leb128()? as usize;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?;
                if idx >= locals.len() {
                    return Err(InterpError::UndefinedLocal(idx as u32));
                }
                locals[idx] = val;
            }

            // local.tee
            0x22 => {
                let idx = reader.read_u32_leb128()? as usize;
                let val = *stack.last().ok_or(InterpError::StackUnderflow)?;
                if idx >= locals.len() {
                    return Err(InterpError::UndefinedLocal(idx as u32));
                }
                locals[idx] = val;
            }

            // global.get
            0x23 => {
                let idx = reader.read_u32_leb128()? as usize;
                let val = *globals
                    .get(idx)
                    .ok_or(InterpError::UndefinedGlobal(idx as u32))?;
                stack.push(val);
            }

            // global.set
            0x24 => {
                let idx = reader.read_u32_leb128()? as usize;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?;
                if idx >= globals.len() {
                    return Err(InterpError::UndefinedGlobal(idx as u32));
                }
                globals[idx] = val;
            }

            // === Constants ===

            // i32.const
            0x41 => {
                let v = reader.read_i32_leb128()?;
                stack.push(Value::I32(v));
            }

            // i64.const
            0x42 => {
                let v = reader.read_i64_leb128()?;
                stack.push(Value::I64(v));
            }

            // f32.const
            0x43 => {
                let v = reader.read_f32()?;
                stack.push(Value::F32(v));
            }

            // f64.const
            0x44 => {
                let v = reader.read_f64()?;
                stack.push(Value::F64(v));
            }

            // === i32 Comparisons ===

            // i32.eqz
            0x45 => {
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a == 0 { 1 } else { 0 }));
            }

            // i32.eq
            0x46 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a == b { 1 } else { 0 }));
            }

            // i32.ne
            0x47 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a != b { 1 } else { 0 }));
            }

            // i32.lt_s
            0x48 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a < b { 1 } else { 0 }));
            }

            // i32.gt_s
            0x4A => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a > b { 1 } else { 0 }));
            }

            // i32.le_s
            0x4C => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a <= b { 1 } else { 0 }));
            }

            // i32.ge_s
            0x4E => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(if a >= b { 1 } else { 0 }));
            }

            // === i64 Comparisons ===

            // i64.eq
            0x51 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a == b { 1 } else { 0 }));
            }

            // i64.ne
            0x52 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a != b { 1 } else { 0 }));
            }

            // i64.lt_s
            0x53 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a < b { 1 } else { 0 }));
            }

            // i64.gt_s
            0x55 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a > b { 1 } else { 0 }));
            }

            // i64.le_s
            0x57 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a <= b { 1 } else { 0 }));
            }

            // i64.ge_s
            0x59 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(if a >= b { 1 } else { 0 }));
            }

            // === f32 Comparisons ===

            // f32.eq
            0x5B => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a == b { 1 } else { 0 }));
            }

            // f32.ne
            0x5C => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a != b { 1 } else { 0 }));
            }

            // f32.lt
            0x5D => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a < b { 1 } else { 0 }));
            }

            // f32.gt
            0x5E => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a > b { 1 } else { 0 }));
            }

            // f32.le
            0x5F => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a <= b { 1 } else { 0 }));
            }

            // f32.ge
            0x60 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(if a >= b { 1 } else { 0 }));
            }

            // === f64 Comparisons ===

            // f64.eq
            0x61 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a == b { 1 } else { 0 }));
            }

            // f64.ne
            0x62 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a != b { 1 } else { 0 }));
            }

            // f64.lt
            0x63 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a < b { 1 } else { 0 }));
            }

            // f64.gt
            0x64 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a > b { 1 } else { 0 }));
            }

            // f64.le
            0x65 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a <= b { 1 } else { 0 }));
            }

            // f64.ge
            0x66 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(if a >= b { 1 } else { 0 }));
            }

            // === i32 Arithmetic ===

            // i32.add
            0x6A => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a.wrapping_add(b)));
            }

            // i32.sub
            0x6B => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a.wrapping_sub(b)));
            }

            // i32.mul
            0x6C => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a.wrapping_mul(b)));
            }

            // i32.div_s
            0x6D => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                if b == 0 {
                    return Err(InterpError::DivisionByZero);
                }
                stack.push(Value::I32(a.wrapping_div(b)));
            }

            // i32.rem_s
            0x6F => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                if b == 0 {
                    return Err(InterpError::DivisionByZero);
                }
                stack.push(Value::I32(a.wrapping_rem(b)));
            }

            // i32.and
            0x71 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a & b));
            }

            // i32.or
            0x72 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a | b));
            }

            // i32.xor
            0x73 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I32(a ^ b));
            }

            // === i64 Arithmetic ===

            // i64.add
            0x7C => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I64(a.wrapping_add(b)));
            }

            // i64.sub
            0x7D => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I64(a.wrapping_sub(b)));
            }

            // i64.mul
            0x7E => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I64(a.wrapping_mul(b)));
            }

            // i64.div_s
            0x7F => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                if b == 0 {
                    return Err(InterpError::DivisionByZero);
                }
                stack.push(Value::I64(a.wrapping_div(b)));
            }

            // i64.rem_s
            0x81 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                if b == 0 {
                    return Err(InterpError::DivisionByZero);
                }
                stack.push(Value::I64(a.wrapping_rem(b)));
            }

            // === f32 Arithmetic ===

            // f32.neg
            0x8C => {
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F32(-a));
            }

            // f32.add
            0x92 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F32(a + b));
            }

            // f32.sub
            0x93 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F32(a - b));
            }

            // f32.mul
            0x94 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F32(a * b));
            }

            // f32.div
            0x95 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F32(a / b));
            }

            // === f64 Arithmetic ===

            // f64.neg
            0x9A => {
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F64(-a));
            }

            // f64.add
            0xA0 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F64(a + b));
            }

            // f64.sub
            0xA1 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F64(a - b));
            }

            // f64.mul
            0xA2 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F64(a * b));
            }

            // f64.div
            0xA3 => {
                let b = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let a = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F64(a / b));
            }

            // === Memory Load/Store ===

            // i32.load
            0x28 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let val = i32::from_le_bytes([memory[addr], memory[addr+1], memory[addr+2], memory[addr+3]]);
                stack.push(Value::I32(val));
            }

            // i64.load
            0x29 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 8 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let val = i64::from_le_bytes([
                    memory[addr], memory[addr+1], memory[addr+2], memory[addr+3],
                    memory[addr+4], memory[addr+5], memory[addr+6], memory[addr+7],
                ]);
                stack.push(Value::I64(val));
            }

            // f32.load
            0x2A => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let val = f32::from_le_bytes([memory[addr], memory[addr+1], memory[addr+2], memory[addr+3]]);
                stack.push(Value::F32(val));
            }

            // f64.load
            0x2B => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 8 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let val = f64::from_le_bytes([
                    memory[addr], memory[addr+1], memory[addr+2], memory[addr+3],
                    memory[addr+4], memory[addr+5], memory[addr+6], memory[addr+7],
                ]);
                stack.push(Value::F64(val));
            }

            // i32.load8_s
            0x2C => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr >= memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                stack.push(Value::I32(memory[addr] as i8 as i32));
            }

            // i32.load8_u
            0x2D => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr >= memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                stack.push(Value::I32(memory[addr] as i32));
            }

            // i32.store
            0x36 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let bytes = val.to_le_bytes();
                memory[addr..addr+4].copy_from_slice(&bytes);
            }

            // i64.store
            0x37 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 8 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let bytes = val.to_le_bytes();
                memory[addr..addr+8].copy_from_slice(&bytes);
            }

            // f32.store
            0x38 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 4 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let bytes = val.to_le_bytes();
                memory[addr..addr+4].copy_from_slice(&bytes);
            }

            // f64.store
            0x39 => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr + 8 > memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                let bytes = val.to_le_bytes();
                memory[addr..addr+8].copy_from_slice(&bytes);
            }

            // i32.store8
            0x3A => {
                let _align = reader.read_u32_leb128()?;
                let offset = reader.read_u32_leb128()?;
                let val = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                let base = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let addr = (base + offset) as usize;
                if addr >= memory.len() {
                    return Err(InterpError::HostError(format!("memory access out of bounds: {}", addr)));
                }
                memory[addr] = val as u8;
            }

            // memory.size (0x3F)
            0x3F => {
                let _reserved = reader.read_byte()?; // memory index (always 0)
                let pages = (memory.len() / 65536) as i32;
                stack.push(Value::I32(pages));
            }

            // memory.grow (0x40)
            0x40 => {
                let _reserved = reader.read_byte()?;
                let delta = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()? as u32;
                let current_pages = (memory.len() / 65536) as i32;
                // WHY: Cap at 256 pages (16MB) for mobile safety
                let new_pages = current_pages as u32 + delta;
                if new_pages > 256 {
                    stack.push(Value::I32(-1)); // failure
                } else {
                    memory.resize(new_pages as usize * 65536, 0);
                    stack.push(Value::I32(current_pages));
                }
            }

            // === Type Conversions ===

            // i32.wrap_i64
            0xA7 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::I32(v as i32));
            }

            // i32.trunc_f32_s
            0xA8 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(v as i32));
            }

            // i32.trunc_f32_u
            0xA9 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(v as u32 as i32));
            }

            // i32.trunc_f64_s
            0xAA => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(v as i32));
            }

            // i32.trunc_f64_u
            0xAB => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I32(v as u32 as i32));
            }

            // i64.extend_i32_s
            0xAC => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I64(v as i64));
            }

            // i64.extend_i32_u
            0xAD => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::I64(v as u32 as i64));
            }

            // i64.trunc_f32_s
            0xAE => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I64(v as i64));
            }

            // i64.trunc_f64_s
            0xB0 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I64(v as i64));
            }

            // f32.convert_i32_s
            0xB2 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::F32(v as f32));
            }

            // f32.convert_i32_u
            0xB3 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::F32(v as u32 as f32));
            }

            // f32.convert_i64_s
            0xB4 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::F32(v as f32));
            }

            // f32.demote_f64
            0xB6 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::F32(v as f32));
            }

            // f64.convert_i32_s
            0xB7 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::F64(v as f64));
            }

            // f64.convert_i32_u
            0xB8 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::F64(v as u32 as f64));
            }

            // f64.convert_i64_s
            0xB9 => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::F64(v as f64));
            }

            // f64.promote_f32
            0xBB => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::F64(v as f64));
            }

            // i32.reinterpret_f32
            0xBC => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f32()?;
                stack.push(Value::I32(v.to_bits() as i32));
            }

            // i64.reinterpret_f64
            0xBD => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_f64()?;
                stack.push(Value::I64(v.to_bits() as i64));
            }

            // f32.reinterpret_i32
            0xBE => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i32()?;
                stack.push(Value::F32(f32::from_bits(v as u32)));
            }

            // f64.reinterpret_i64
            0xBF => {
                let v = stack.pop().ok_or(InterpError::StackUnderflow)?.as_i64()?;
                stack.push(Value::F64(f64::from_bits(v as u64)));
            }

            // Unknown opcode — skip gracefully for forward compatibility.
            unknown => {
                return Err(InterpError::InvalidBytecode(format!(
                    "unsupported opcode: 0x{:02X}",
                    unknown
                )));
            }
        }
    }

    Ok(())
}

/// Skip forward in the instruction stream to the matching else or end opcode.
/// Handles nested blocks correctly by tracking depth.
fn skip_to_else_or_end(reader: &mut Reader) -> Result<(), InterpError> {
    let mut depth: u32 = 0;
    while reader.remaining() > 0 {
        let opcode = reader.read_byte()?;
        match opcode {
            // Block-opening opcodes increase nesting depth.
            0x02 | 0x03 | 0x04 => {
                reader.read_byte()?; // block type
                depth += 1;
            }
            // else at our nesting level means we found the else branch.
            0x05 if depth == 0 => return Ok(()),
            // end at our nesting level means the if had no else.
            0x0B if depth == 0 => return Ok(()),
            // end at deeper nesting means we close a nested block.
            0x0B => {
                depth -= 1;
            }
            // Skip operands for instructions that have immediate arguments.
            0x0C | 0x0D => {
                reader.read_u32_leb128()?;
            }
            0x10 => {
                reader.read_u32_leb128()?;
            }
            0x20 | 0x21 | 0x22 | 0x23 | 0x24 => {
                reader.read_u32_leb128()?;
            }
            0x28..=0x2D | 0x36..=0x3A => {
                // Memory load/store — align + offset
                reader.read_u32_leb128()?;
                reader.read_u32_leb128()?;
            }
            0x3F | 0x40 => {
                // memory.size / memory.grow — reserved byte
                reader.read_byte()?;
            }
            0x41 => {
                reader.read_i32_leb128()?;
            }
            0x42 => {
                reader.read_i64_leb128()?;
            }
            0x43 => {
                reader.read_bytes(4)?;
            }
            0x44 => {
                reader.read_bytes(8)?;
            }
            _ => {}
        }
    }
    Err(InterpError::InvalidBytecode(
        "unexpected end of block while skipping".into(),
    ))
}

/// Skip to the matching end opcode, handling nesting.
fn skip_to_end(reader: &mut Reader) -> Result<(), InterpError> {
    let mut depth: u32 = 0;
    while reader.remaining() > 0 {
        let opcode = reader.read_byte()?;
        match opcode {
            0x02 | 0x03 | 0x04 => {
                reader.read_byte()?;
                depth += 1;
            }
            0x0B if depth == 0 => return Ok(()),
            0x0B => {
                depth -= 1;
            }
            0x0C | 0x0D => {
                reader.read_u32_leb128()?;
            }
            0x10 => {
                reader.read_u32_leb128()?;
            }
            0x20 | 0x21 | 0x22 | 0x23 | 0x24 => {
                reader.read_u32_leb128()?;
            }
            0x41 => {
                reader.read_i32_leb128()?;
            }
            0x42 => {
                reader.read_i64_leb128()?;
            }
            0x43 => {
                reader.read_bytes(4)?;
            }
            0x44 => {
                reader.read_bytes(8)?;
            }
            _ => {}
        }
    }
    Err(InterpError::InvalidBytecode(
        "unexpected end of block while skipping to end".into(),
    ))
}

// ============================================================================
// Host Function Dispatch
// ============================================================================

/// Read a UTF-8 string from linear memory at the given pointer and length.
fn read_string_from_memory(memory: &[u8], ptr: u32, len: u32) -> Result<String, InterpError> {
    let start = ptr as usize;
    let end = start + len as usize;
    if end > memory.len() {
        return Err(InterpError::HostError(format!(
            "string read out of bounds: ptr={}, len={}, mem_size={}",
            ptr, len, memory.len()
        )));
    }
    String::from_utf8(memory[start..end].to_vec())
        .map_err(|_| InterpError::HostError("invalid UTF-8 in string from memory".into()))
}

/// Dispatch a call to an imported host function.
fn dispatch_host_call(
    module: &WasmModule,
    func_idx: u32,
    args: &[Value],
    memory: &mut Vec<u8>,
    gas_meter: &mut GasMeter,
    host_env: &mut HostEnvironment,
    permissions: &ContractPermissions,
) -> Result<Option<Value>, InterpError> {
    let import = &module.imports[func_idx as usize];
    let name = &import.name;

    // Charge gas for the host call.
    gas_meter
        .charge_host_call(name)
        .map_err(InterpError::Gas)?;

    // Check permissions.
    permissions
        .check_permission(name)
        .map_err(|e| InterpError::HostError(e.to_string()))?;

    match name.as_str() {
        "get_location_lat" => {
            let loc = host_env.get_location();
            Ok(Some(Value::F32(loc.map(|(lat, _)| lat).unwrap_or(0.0))))
        }
        "get_location_lon" => {
            let loc = host_env.get_location();
            Ok(Some(Value::F32(loc.map(|(_, lon)| lon).unwrap_or(0.0))))
        }
        "get_nearby_peers" => {
            let count = host_env.get_nearby_peers();
            Ok(Some(Value::I32(count as i32)))
        }
        "get_presence_score" => {
            let score = host_env.get_presence_score();
            Ok(Some(Value::I32(score as i32)))
        }
        "get_sensor_data" => {
            // WHY: The sensor type is passed as an i32 argument.
            // Mapping: 0=Barometer, 1=AmbientLight, 2=Magnetometer,
            // 3=Accelerometer, 4=Gyroscope.
            use crate::host_functions::SensorType;
            let sensor_idx = args.first().map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0);
            let sensor_type = match sensor_idx {
                0 => SensorType::Barometer,
                1 => SensorType::AmbientLight,
                2 => SensorType::Magnetometer,
                3 => SensorType::Accelerometer,
                4 => SensorType::Gyroscope,
                _ => return Ok(Some(Value::F64(0.0))),
            };
            let reading = host_env.get_sensor_data(sensor_type);
            Ok(Some(Value::F64(reading.map(|r| r.value).unwrap_or(0.0))))
        }
        "get_block_height" => {
            let height = host_env.get_block_height();
            Ok(Some(Value::I64(height as i64)))
        }
        "get_block_timestamp" => {
            let ts = host_env.get_block_timestamp();
            Ok(Some(Value::I64(ts as i64)))
        }
        "get_caller_address" => {
            // Write the 32-byte address into linear memory and return the pointer.
            // WHY: Addresses are 32 bytes — too large for a single WASM value.
            // We write to a fixed location in memory and return the pointer.
            let addr = host_env.get_caller_address();
            // Write at offset 0 in memory (reserved area for host return data)
            if memory.len() >= 32 {
                memory[0..32].copy_from_slice(&addr.0);
            }
            Ok(Some(Value::I32(0))) // pointer to start of memory
        }
        "get_caller_balance" => {
            let balance = host_env.get_caller_balance();
            Ok(Some(Value::I64(balance as i64)))
        }
        "storage_read" => {
            // storage_read(key_ptr: i32, key_len: i32) -> i32 (value_ptr or 0 if not found)
            // WHY: Reads a value from contract storage by key. The key bytes are
            // read from linear memory. If found, the value is written to memory
            // and the pointer is returned; if not found, returns 0.
            let key_ptr = args.first().map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let key_len = args.get(1).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;

            // Read key bytes from memory
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len.min(32) as usize;
            if key_end > memory.len() {
                return Ok(Some(Value::I32(0)));
            }
            let mut storage_key = [0u8; 32];
            let slice_len = (key_end - key_start).min(32);
            storage_key[..slice_len].copy_from_slice(&memory[key_start..key_start + slice_len]);

            if let Some(value) = host_env.storage_read(&storage_key) {
                // Write value to memory at a known location (after 1024 byte reserved area)
                let write_offset = 1024usize;
                let val_len = value.len();
                if write_offset + 4 + val_len <= memory.len() {
                    // Write length first (4 bytes LE), then data
                    memory[write_offset..write_offset + 4].copy_from_slice(&(val_len as u32).to_le_bytes());
                    memory[write_offset + 4..write_offset + 4 + val_len].copy_from_slice(value);
                    Ok(Some(Value::I32(write_offset as i32)))
                } else {
                    Ok(Some(Value::I32(0)))
                }
            } else {
                Ok(Some(Value::I32(0)))
            }
        }
        "storage_write" => {
            // storage_write(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> void
            // WHY: Writes a key-value pair to contract storage. Both key and value
            // are read from linear memory at the specified pointers.
            let key_ptr = args.first().map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let key_len = args.get(1).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let val_ptr = args.get(2).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let val_len = args.get(3).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;

            // Read key from memory
            let key_start = key_ptr as usize;
            let key_end = key_start + key_len.min(32) as usize;
            if key_end > memory.len() {
                return Err(InterpError::HostError("storage_write: key out of bounds".into()));
            }
            let mut storage_key = [0u8; 32];
            let slice_len = (key_end - key_start).min(32);
            storage_key[..slice_len].copy_from_slice(&memory[key_start..key_start + slice_len]);

            // Read value from memory
            let val_start = val_ptr as usize;
            let val_end = val_start + val_len as usize;
            if val_end > memory.len() {
                return Err(InterpError::HostError("storage_write: value out of bounds".into()));
            }
            let value = memory[val_start..val_end].to_vec();

            host_env.storage_write(storage_key, value);
            Ok(None)
        }
        "emit_event" => {
            // emit_event(topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32) -> void
            // WHY: Emits a contract event with a topic string and data bytes,
            // both read from linear memory.
            let topic_ptr = args.first().map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let topic_len = args.get(1).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let data_ptr = args.get(2).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;
            let data_len = args.get(3).map(|v| v.as_i32().unwrap_or(0)).unwrap_or(0) as u32;

            let topic = read_string_from_memory(memory, topic_ptr, topic_len)?;

            let d_start = data_ptr as usize;
            let d_end = d_start + data_len as usize;
            if d_end > memory.len() {
                return Err(InterpError::HostError("emit_event: data out of bounds".into()));
            }
            let data = memory[d_start..d_end].to_vec();

            // Use a zero address as the contract address — the VM layer fills in
            // the real contract address from the call context.
            host_env.emit_event(Address([0u8; 32]), topic, data);
            Ok(None)
        }
        _ => {
            // Unknown host function — return void.
            tracing::warn!(
                function = name.as_str(),
                "unknown host function called, returning void"
            );
            Ok(None)
        }
    }
}

// ============================================================================
// Contract Value Conversion
// ============================================================================

fn contract_value_to_value(cv: &ContractValue) -> Value {
    match cv {
        ContractValue::I32(v) => Value::I32(*v),
        ContractValue::I64(v) => Value::I64(*v),
        ContractValue::F32(v) => Value::F32(*v),
        ContractValue::F64(v) => Value::F64(*v),
        ContractValue::Bool(b) => Value::I32(if *b { 1 } else { 0 }),
        // Complex types not yet supported in the interpreter — use zero placeholder.
        _ => Value::I32(0),
    }
}

// ============================================================================
// InterpreterRuntime — ContractRuntime Implementation
// ============================================================================

/// A loaded contract in the interpreter: parsed module + mutable global state.
struct LoadedContract {
    module: WasmModule,
    /// Global variable values (mutable state).
    globals: Vec<Value>,
    /// Linear memory (initialized from memory section + data segments).
    memory: Vec<u8>,
    /// SHA-256 hash of the bytecode.
    bytecode_hash: [u8; 32],
}

/// Pure-Rust WASM interpreter implementing `ContractRuntime`.
///
/// This runtime parses and interprets WASM bytecode directly, without
/// any external WASM engine. It supports exactly the instruction set
/// that the GratiaScript compiler emits.
pub struct InterpreterRuntime {
    /// Loaded contracts keyed by address.
    contracts: HashMap<Address, LoadedContract>,
}

impl InterpreterRuntime {
    pub fn new() -> Self {
        InterpreterRuntime {
            contracts: HashMap::new(),
        }
    }
}

impl Default for InterpreterRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractRuntime for InterpreterRuntime {
    fn load_contract(
        &mut self,
        contract_address: Address,
        bytecode: &[u8],
        config: &SandboxConfig,
    ) -> Result<(), RuntimeError> {
        // Validate bytecode structure.
        validate_bytecode(bytecode, config)?;

        // Parse the WASM binary into our internal representation.
        let module = parse_module(bytecode).map_err(|e| RuntimeError::CompilationFailed {
            reason: format!("{}", e),
        })?;

        // Initialize globals from their init expressions.
        let globals: Vec<Value> = module.globals.iter().map(|g| g.init_value).collect();

        // Initialize linear memory.
        // WHY: WASM linear memory starts as zeroed pages. Data segments then
        // write initial values (string literals, etc.) into the memory.
        let mem_size = (module.memory_min_pages.max(1) as usize) * 65536;
        let mut memory = vec![0u8; mem_size];
        for seg in &module.data_segments {
            let start = seg.offset as usize;
            let end = start + seg.data.len();
            if end <= memory.len() {
                memory[start..end].copy_from_slice(&seg.data);
            }
        }

        // Compute bytecode hash.
        let mut hasher = Sha256::new();
        hasher.update(bytecode);
        let result = hasher.finalize();
        let mut bytecode_hash = [0u8; 32];
        bytecode_hash.copy_from_slice(&result);

        self.contracts.insert(
            contract_address,
            LoadedContract {
                module,
                globals,
                memory,
                bytecode_hash,
            },
        );

        tracing::debug!(
            address = %contract_address,
            bytecode_size = bytecode.len(),
            "Interpreter runtime loaded contract"
        );

        Ok(())
    }

    fn execute_contract(
        &mut self,
        contract_address: &Address,
        function_name: &str,
        args: &[ContractValue],
        gas_meter: &mut GasMeter,
        host_env: &mut HostEnvironment,
        permissions: &ContractPermissions,
    ) -> Result<ExecutionOutcome, RuntimeError> {
        let contract = self
            .contracts
            .get_mut(contract_address)
            .ok_or_else(|| RuntimeError::ContractNotFound {
                address: format!("{}", contract_address),
            })?;

        // Find the exported function by name.
        let export = contract
            .module
            .exports
            .iter()
            .find(|e| e.name == function_name && e.kind == ExportKind::Function)
            .ok_or_else(|| RuntimeError::FunctionNotFound {
                name: function_name.to_string(),
            })?;

        let func_idx = export.index;

        // Convert ContractValue args to interpreter Values.
        let value_args: Vec<Value> = args.iter().map(contract_value_to_value).collect();

        // Charge base call gas.
        gas_meter
            .charge_host_call("base_call")
            .map_err(RuntimeError::Gas)?;

        // Execute the function.
        let module = &contract.module;
        let globals = &mut contract.globals;
        let memory = &mut contract.memory;

        let result = execute_function(
            module,
            func_idx,
            &value_args,
            globals,
            memory,
            gas_meter,
            host_env,
            permissions,
            0,
        );

        match result {
            Ok(Some(val)) => Ok(ExecutionOutcome {
                return_value: val.to_contract_value(),
                gas_used: gas_meter.gas_used(),
                success: true,
                error: None,
            }),
            Ok(None) => Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: true,
                error: None,
            }),
            Err(InterpError::Gas(gas_err)) => Err(RuntimeError::Gas(gas_err)),
            Err(e) => Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: false,
                error: Some(format!("{}", e)),
            }),
        }
    }

    fn is_loaded(&self, contract_address: &Address) -> bool {
        self.contracts.contains_key(contract_address)
    }

    fn unload_contract(&mut self, contract_address: &Address) {
        self.contracts.remove(contract_address);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gas::GasMeter;
    use crate::host_functions::HostEnvironment;
    use crate::sandbox::{ContractPermissions, SandboxConfig};
    use gratia_core::types::{Address, GeoLocation};

    fn valid_wasm_bytecode() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    fn make_test_env() -> HostEnvironment {
        HostEnvironment::new(100, 1700000000, Address([1u8; 32]), 5_000_000)
    }

    fn make_test_env_with_location() -> HostEnvironment {
        HostEnvironment::new(100, 1700000000, Address([1u8; 32]), 5_000_000)
            .with_location(GeoLocation {
                lat: 37.7749,
                lon: -122.4194,
            })
            .with_nearby_peers(12)
            .with_presence_score(75)
    }

    #[test]
    fn test_interpreter_load_and_check() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        assert!(!runtime.is_loaded(&addr));

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        assert!(runtime.is_loaded(&addr));
    }

    #[test]
    fn test_interpreter_load_invalid_bytecode() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bad_bytecode = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0x00];

        let result = runtime.load_contract(addr, &bad_bytecode, &SandboxConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_interpreter_unload() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();
        assert!(runtime.is_loaded(&addr));

        runtime.unload_contract(&addr);
        assert!(!runtime.is_loaded(&addr));
    }

    #[test]
    fn test_interpreter_execute_not_loaded() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "func", &[], &mut gas_meter, &mut env, &perms);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RuntimeError::ContractNotFound { .. }
        ));
    }

    // --- LEB128 encoding helpers for building WASM bytecode in tests ---

    fn encode_u32_leb128(out: &mut Vec<u8>, mut val: u32) {
        loop {
            let mut byte = (val & 0x7F) as u8;
            val >>= 7;
            if val != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if val == 0 {
                break;
            }
        }
    }

    fn encode_i32_leb128(out: &mut Vec<u8>, mut val: i32) {
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

    fn encode_str(out: &mut Vec<u8>, s: &str) {
        encode_u32_leb128(out, s.len() as u32);
        out.extend_from_slice(s.as_bytes());
    }

    fn emit_section(out: &mut Vec<u8>, section_id: u8, body: &[u8]) {
        out.push(section_id);
        encode_u32_leb128(out, body.len() as u32);
        out.extend_from_slice(body);
    }

    /// Build a minimal WASM module with a single function that returns i32(42).
    fn build_return_42_wasm() -> Vec<u8> {
        let mut wasm = Vec::new();

        // Magic + version
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: one func type () -> i32
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1); // 1 type
            body.push(0x60); // func
            encode_u32_leb128(&mut body, 0); // 0 params
            encode_u32_leb128(&mut body, 1); // 1 result
            body.push(0x7F); // i32
            emit_section(&mut wasm, 1, &body);
        }

        // Function section: 1 function, type index 0
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        // Export section: export function 0 as "answer"
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1); // 1 export
            encode_str(&mut body, "answer");
            body.push(0x00); // function
            encode_u32_leb128(&mut body, 0); // index 0
            emit_section(&mut wasm, 7, &body);
        }

        // Code section: function body
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0); // 0 local declarations
            func_body.push(0x41); // i32.const
            encode_i32_leb128(&mut func_body, 42);
            func_body.push(0x0B); // end

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1); // 1 function body
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        wasm
    }

    #[test]
    fn test_interpreter_return_constant() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_return_42_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "answer", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
        assert!(result.gas_used > 0);
    }

    /// Build a WASM module with a function (a: i32, b: i32) -> i32 that adds them.
    fn build_add_wasm() -> Vec<u8> {
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: (i32, i32) -> i32
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x60);
            encode_u32_leb128(&mut body, 2); // 2 params
            body.push(0x7F); // i32
            body.push(0x7F); // i32
            encode_u32_leb128(&mut body, 1); // 1 result
            body.push(0x7F); // i32
            emit_section(&mut wasm, 1, &body);
        }

        // Function section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        // Export section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_str(&mut body, "add");
            body.push(0x00);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 7, &body);
        }

        // Code section: local.get 0, local.get 1, i32.add, end
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x20); // local.get
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x20); // local.get
            encode_u32_leb128(&mut func_body, 1);
            func_body.push(0x6A); // i32.add
            func_body.push(0x0B); // end

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        wasm
    }

    #[test]
    fn test_interpreter_add_function() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_add_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(
                &addr,
                "add",
                &[ContractValue::I32(17), ContractValue::I32(25)],
                &mut gas_meter,
                &mut env,
                &perms,
            )
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
    }

    /// Build a WASM module with a global variable and a getter.
    fn build_global_wasm() -> Vec<u8> {
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: () -> i32
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x60);
            encode_u32_leb128(&mut body, 0);
            encode_u32_leb128(&mut body, 1);
            body.push(0x7F);
            emit_section(&mut wasm, 1, &body);
        }

        // Function section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        // Global section: mutable i32 initialized to 99
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x7F); // i32
            body.push(0x01); // mutable
            body.push(0x41); // i32.const
            encode_i32_leb128(&mut body, 99);
            body.push(0x0B); // end
            emit_section(&mut wasm, 6, &body);
        }

        // Export section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_str(&mut body, "getVal");
            body.push(0x00);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 7, &body);
        }

        // Code section: global.get 0, end
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x23); // global.get
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x0B); // end

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        wasm
    }

    #[test]
    fn test_interpreter_global_variable() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_global_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "getVal", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(99));
    }

    /// Build a WASM module with an if/else: returns 1 if arg > 10, else 0.
    fn build_if_else_wasm() -> Vec<u8> {
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: (i32) -> i32
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x60);
            encode_u32_leb128(&mut body, 1);
            body.push(0x7F);
            encode_u32_leb128(&mut body, 1);
            body.push(0x7F);
            emit_section(&mut wasm, 1, &body);
        }

        // Function section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        // Export section
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_str(&mut body, "check");
            body.push(0x00);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 7, &body);
        }

        // Code section: local.get 0, i32.const 10, i32.gt_s, if void, i32.const 1, return, else, i32.const 0, return, end, i32.const 0, end
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0); // no extra locals

            // local.get 0
            func_body.push(0x20);
            encode_u32_leb128(&mut func_body, 0);
            // i32.const 10
            func_body.push(0x41);
            encode_i32_leb128(&mut func_body, 10);
            // i32.gt_s
            func_body.push(0x4A);
            // if void
            func_body.push(0x04);
            func_body.push(0x40);
            // i32.const 1
            func_body.push(0x41);
            encode_i32_leb128(&mut func_body, 1);
            // return
            func_body.push(0x0F);
            // else
            func_body.push(0x05);
            // i32.const 0
            func_body.push(0x41);
            encode_i32_leb128(&mut func_body, 0);
            // return
            func_body.push(0x0F);
            // end (if)
            func_body.push(0x0B);
            // i32.const 0 (fallback — should never reach here)
            func_body.push(0x41);
            encode_i32_leb128(&mut func_body, 0);
            // end (function)
            func_body.push(0x0B);

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        wasm
    }

    #[test]
    fn test_interpreter_if_else() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_if_else_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        // Test: arg > 10 should return 1
        {
            let mut gas_meter = GasMeter::new(1_000_000);
            let mut env = make_test_env();
            let perms = ContractPermissions::default();

            let result = runtime
                .execute_contract(
                    &addr,
                    "check",
                    &[ContractValue::I32(20)],
                    &mut gas_meter,
                    &mut env,
                    &perms,
                )
                .unwrap();

            assert!(result.success);
            assert_eq!(result.return_value, ContractValue::I32(1));
        }

        // Test: arg <= 10 should return 0
        {
            let mut gas_meter = GasMeter::new(1_000_000);
            let mut env = make_test_env();
            let perms = ContractPermissions::default();

            let result = runtime
                .execute_contract(
                    &addr,
                    "check",
                    &[ContractValue::I32(5)],
                    &mut gas_meter,
                    &mut env,
                    &perms,
                )
                .unwrap();

            assert!(result.success);
            assert_eq!(result.return_value, ContractValue::I32(0));
        }
    }

    #[test]
    fn test_interpreter_gas_metering() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_return_42_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let gas_before = gas_meter.gas_used();
        runtime
            .execute_contract(&addr, "answer", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();
        let gas_after = gas_meter.gas_used();

        // Gas should have been consumed.
        assert!(gas_after > gas_before);
    }

    #[test]
    fn test_interpreter_out_of_gas() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_return_42_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        // WHY: Gas limit of 1 is not enough for even the base call charge.
        let mut gas_meter = GasMeter::new(1);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "answer", &[], &mut gas_meter, &mut env, &perms);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RuntimeError::Gas(_)));
    }

    #[test]
    fn test_interpreter_function_not_found() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_return_42_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime.execute_contract(
            &addr,
            "nonexistent",
            &[],
            &mut gas_meter,
            &mut env,
            &perms,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RuntimeError::FunctionNotFound { .. }
        ));
    }

    #[test]
    fn test_interpreter_f32_arithmetic() {
        // Build a module: (f32, f32) -> f32, computes a * b + a
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: (f32, f32) -> f32
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x60);
            encode_u32_leb128(&mut body, 2);
            body.push(0x7D); // f32
            body.push(0x7D);
            encode_u32_leb128(&mut body, 1);
            body.push(0x7D);
            emit_section(&mut wasm, 1, &body);
        }

        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_str(&mut body, "calc");
            body.push(0x00);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 7, &body);
        }

        // Code: local.get 0, local.get 1, f32.mul, local.get 0, f32.add, end
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x20);
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x20);
            encode_u32_leb128(&mut func_body, 1);
            func_body.push(0x94); // f32.mul
            func_body.push(0x20);
            encode_u32_leb128(&mut func_body, 0);
            func_body.push(0x92); // f32.add
            func_body.push(0x0B);

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);

        runtime
            .load_contract(addr, &wasm, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(
                &addr,
                "calc",
                &[ContractValue::F32(3.0), ContractValue::F32(4.0)],
                &mut gas_meter,
                &mut env,
                &perms,
            )
            .unwrap();

        assert!(result.success);
        // 3.0 * 4.0 + 3.0 = 15.0
        match result.return_value {
            ContractValue::F32(v) => assert!((v - 15.0).abs() < 0.001),
            _ => panic!("expected f32 return value"),
        }
    }

    /// Build a WASM module with a host function import (get_nearby_peers).
    fn build_host_call_wasm() -> Vec<u8> {
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00]);

        // Type section: two types
        // Type 0: () -> i32 (for the import and the exported function)
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            body.push(0x60);
            encode_u32_leb128(&mut body, 0);
            encode_u32_leb128(&mut body, 1);
            body.push(0x7F);
            emit_section(&mut wasm, 1, &body);
        }

        // Import section: import get_nearby_peers from "env"
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1); // 1 import
            encode_str(&mut body, "env");
            encode_str(&mut body, "get_nearby_peers");
            body.push(0x00); // function
            encode_u32_leb128(&mut body, 0); // type index 0
            emit_section(&mut wasm, 2, &body);
        }

        // Function section: 1 function, type 0
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, 0);
            emit_section(&mut wasm, 3, &body);
        }

        // Export section: export function 1 (after 1 import) as "getPeers"
        {
            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_str(&mut body, "getPeers");
            body.push(0x00);
            encode_u32_leb128(&mut body, 1); // function index 1 (0 is the import)
            emit_section(&mut wasm, 7, &body);
        }

        // Code section: call 0 (the import), end
        {
            let mut func_body = Vec::new();
            encode_u32_leb128(&mut func_body, 0); // no locals
            func_body.push(0x10); // call
            encode_u32_leb128(&mut func_body, 0); // function index 0 = get_nearby_peers
            func_body.push(0x0B); // end

            let mut body = Vec::new();
            encode_u32_leb128(&mut body, 1);
            encode_u32_leb128(&mut body, func_body.len() as u32);
            body.extend_from_slice(&func_body);
            emit_section(&mut wasm, 10, &body);
        }

        wasm
    }

    #[test]
    fn test_interpreter_host_call() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_host_call_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env_with_location();
        let perms = ContractPermissions {
            proximity: true,
            ..Default::default()
        };

        let result = runtime
            .execute_contract(&addr, "getPeers", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(12));
    }

    #[test]
    fn test_interpreter_host_call_permission_denied() {
        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = build_host_call_wasm();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env_with_location();
        // WHY: Default permissions deny proximity. The host call should fail.
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "getPeers", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        // Execution completes but with an error.
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("permission"));
    }

    #[test]
    fn test_value_conversions() {
        assert_eq!(Value::I32(42).as_i32().unwrap(), 42);
        assert_eq!(Value::I64(100).as_i64().unwrap(), 100);
        assert!((Value::F32(3.14).as_f32().unwrap() - 3.14).abs() < 0.01);
        assert!((Value::F64(2.718).as_f64().unwrap() - 2.718).abs() < 0.001);

        // Type mismatch
        assert!(Value::I32(42).as_i64().is_err());
        assert!(Value::F32(1.0).as_i32().is_err());
    }

    #[test]
    fn test_leb128_round_trip() {
        let mut buf = Vec::new();
        encode_u32_leb128(&mut buf, 300);

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.read_u32_leb128().unwrap(), 300);

        let mut buf = Vec::new();
        encode_i32_leb128(&mut buf, -42);

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.read_i32_leb128().unwrap(), -42);
    }

    #[test]
    fn test_parse_empty_module() {
        let bytecode = valid_wasm_bytecode();
        let module = parse_module(&bytecode).unwrap();
        assert!(module.types.is_empty());
        assert!(module.imports.is_empty());
        assert!(module.functions.is_empty());
        assert!(module.globals.is_empty());
        assert!(module.exports.is_empty());
    }

    #[test]
    fn test_memory_load_store() {
        // Build a WASM module that: stores 42 at memory[0], loads it back, returns it
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: () -> (i32)
        let type_section: Vec<u8> = vec![0x01, 0x60, 0x00, 0x01, 0x7F];
        bytecode.push(1); // section id
        encode_u32_leb128(&mut bytecode, type_section.len() as u32);
        bytecode.extend_from_slice(&type_section);

        // Function section: 1 function, type index 0
        let func_section = vec![0x01, 0x00];
        bytecode.push(3);
        encode_u32_leb128(&mut bytecode, func_section.len() as u32);
        bytecode.extend_from_slice(&func_section);

        // Memory section: 1 memory, min 1 page
        let mem_section = vec![0x01, 0x00, 0x01];
        bytecode.push(5);
        encode_u32_leb128(&mut bytecode, mem_section.len() as u32);
        bytecode.extend_from_slice(&mem_section);

        // Export section: export "test" as function 0
        let mut export_section = Vec::new();
        export_section.push(0x01); // 1 export
        export_section.push(0x04); // name length
        export_section.extend_from_slice(b"test");
        export_section.push(0x00); // function export
        export_section.push(0x00); // function index 0
        bytecode.push(7);
        encode_u32_leb128(&mut bytecode, export_section.len() as u32);
        bytecode.extend_from_slice(&export_section);

        // Code section: function body
        let mut body = Vec::new();
        body.push(0x00); // 0 local declarations
        // i32.const 0 (base address)
        body.push(0x41); body.push(0x00);
        // i32.const 42 (value)
        body.push(0x41); body.push(0x2A);
        // i32.store align=2 offset=0
        body.push(0x36); body.push(0x02); body.push(0x00);
        // i32.const 0 (address to load from)
        body.push(0x41); body.push(0x00);
        // i32.load align=2 offset=0
        body.push(0x28); body.push(0x02); body.push(0x00);
        // end
        body.push(0x0B);

        let mut code_section = Vec::new();
        code_section.push(0x01); // 1 function
        encode_u32_leb128(&mut code_section, body.len() as u32);
        code_section.extend_from_slice(&body);
        bytecode.push(10);
        encode_u32_leb128(&mut bytecode, code_section.len() as u32);
        bytecode.extend_from_slice(&code_section);

        let mut runtime = InterpreterRuntime::new();
        let addr = Address([1u8; 32]);
        runtime.load_contract(addr, &bytecode, &SandboxConfig::default()).unwrap();

        let mut gas = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();
        let result = runtime.execute_contract(&addr, "test", &[], &mut gas, &mut env, &perms).unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
    }

    #[test]
    fn test_storage_write_and_read_via_host() {
        // Test that storage_write and storage_read host functions work
        let mut env = make_test_env();

        // Write directly to storage
        let mut key = [0u8; 32];
        key[0] = 0x48; // 'H'
        key[1] = 0x69; // 'i'
        env.storage_write(key, vec![1, 2, 3, 4]);

        // Verify it can be read back
        let val = env.storage_read(&key).unwrap();
        assert_eq!(val, &vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_type_conversion_i32_to_f32() {
        // Build a WASM module: takes i32 arg, converts to f32, returns f32
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: (i32) -> (f32)
        let type_section: Vec<u8> = vec![0x01, 0x60, 0x01, 0x7F, 0x01, 0x7D];
        bytecode.push(1);
        encode_u32_leb128(&mut bytecode, type_section.len() as u32);
        bytecode.extend_from_slice(&type_section);

        // Function section
        let func_section = vec![0x01, 0x00];
        bytecode.push(3);
        encode_u32_leb128(&mut bytecode, func_section.len() as u32);
        bytecode.extend_from_slice(&func_section);

        // Memory section
        let mem_section = vec![0x01, 0x00, 0x01];
        bytecode.push(5);
        encode_u32_leb128(&mut bytecode, mem_section.len() as u32);
        bytecode.extend_from_slice(&mem_section);

        // Export section
        let mut export_section = Vec::new();
        export_section.push(0x01);
        export_section.push(0x07);
        export_section.extend_from_slice(b"convert");
        export_section.push(0x00);
        export_section.push(0x00);
        bytecode.push(7);
        encode_u32_leb128(&mut bytecode, export_section.len() as u32);
        bytecode.extend_from_slice(&export_section);

        // Code section: local.get 0, f32.convert_i32_s (0xB2), end
        let mut body = Vec::new();
        body.push(0x00); // 0 locals
        body.push(0x20); body.push(0x00); // local.get 0
        body.push(0xB2); // f32.convert_i32_s
        body.push(0x0B); // end

        let mut code_section = Vec::new();
        code_section.push(0x01);
        encode_u32_leb128(&mut code_section, body.len() as u32);
        code_section.extend_from_slice(&body);
        bytecode.push(10);
        encode_u32_leb128(&mut bytecode, code_section.len() as u32);
        bytecode.extend_from_slice(&code_section);

        let mut runtime = InterpreterRuntime::new();
        let addr = Address([2u8; 32]);
        runtime.load_contract(addr, &bytecode, &SandboxConfig::default()).unwrap();

        let mut gas = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();
        let result = runtime.execute_contract(
            &addr, "convert", &[ContractValue::I32(42)],
            &mut gas, &mut env, &perms
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::F32(42.0));
    }

    #[test]
    fn test_data_section_initializes_memory() {
        // Build a WASM module with a data section that writes "Hello" at offset 100
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: () -> (i32)
        let type_section: Vec<u8> = vec![0x01, 0x60, 0x00, 0x01, 0x7F];
        bytecode.push(1);
        encode_u32_leb128(&mut bytecode, type_section.len() as u32);
        bytecode.extend_from_slice(&type_section);

        // Function section
        let func_section = vec![0x01, 0x00];
        bytecode.push(3);
        encode_u32_leb128(&mut bytecode, func_section.len() as u32);
        bytecode.extend_from_slice(&func_section);

        // Memory section: 1 page
        let mem_section = vec![0x01, 0x00, 0x01];
        bytecode.push(5);
        encode_u32_leb128(&mut bytecode, mem_section.len() as u32);
        bytecode.extend_from_slice(&mem_section);

        // Export section
        let mut export_section = Vec::new();
        export_section.push(0x01);
        export_section.push(0x04);
        export_section.extend_from_slice(b"read");
        export_section.push(0x00);
        export_section.push(0x00);
        bytecode.push(7);
        encode_u32_leb128(&mut bytecode, export_section.len() as u32);
        bytecode.extend_from_slice(&export_section);

        // Code section: load i32 from memory at offset 100
        let mut body = Vec::new();
        body.push(0x00); // 0 locals
        body.push(0x41); encode_i32_leb128(&mut body, 100); // i32.const 100
        body.push(0x2D); body.push(0x00); body.push(0x00); // i32.load8_u align=0 offset=0
        body.push(0x0B);

        let mut code_section = Vec::new();
        code_section.push(0x01);
        encode_u32_leb128(&mut code_section, body.len() as u32);
        code_section.extend_from_slice(&body);
        bytecode.push(10);
        encode_u32_leb128(&mut bytecode, code_section.len() as u32);
        bytecode.extend_from_slice(&code_section);

        // Data section: write "Hello" at offset 100
        let mut data_section = Vec::new();
        data_section.push(0x01); // 1 segment
        data_section.push(0x00); // active, memory 0
        // offset expression: i32.const 100, end
        data_section.push(0x41); encode_i32_leb128(&mut data_section, 100);
        data_section.push(0x0B);
        // data: "Hello" (5 bytes)
        data_section.push(0x05);
        data_section.extend_from_slice(b"Hello");
        bytecode.push(11);
        encode_u32_leb128(&mut bytecode, data_section.len() as u32);
        bytecode.extend_from_slice(&data_section);

        let mut runtime = InterpreterRuntime::new();
        let addr = Address([3u8; 32]);
        runtime.load_contract(addr, &bytecode, &SandboxConfig::default()).unwrap();

        let mut gas = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();
        let result = runtime.execute_contract(&addr, "read", &[], &mut gas, &mut env, &perms).unwrap();

        assert!(result.success);
        // 'H' = 72
        assert_eq!(result.return_value, ContractValue::I32(72));
    }

}
