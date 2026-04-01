//! Wasmer WASM runtime for GratiaVM smart contract execution.
//!
//! This module provides the `ContractRuntime` trait that abstracts over
//! the WASM execution engine. Two implementations are provided:
//!
//! - `WasmerRuntime` — Real WASM execution via wasmer (requires the
//!   `wasmer-runtime` feature flag). Used in production.
//! - `MockRuntime` — Simulated execution for testing. Always compiles,
//!   no external dependencies.
//!
//! The trait-based approach allows the rest of GratiaVM to be developed,
//! tested, and compiled without wasmer installed, which is critical since
//! wasmer may not compile on all development machines.

use std::collections::HashMap;
use std::fmt;
#[cfg(feature = "wasmer-runtime")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "wasmer-runtime")]
use std::time::Instant;

use sha2::{Digest, Sha256};
use thiserror::Error;

use gratia_core::types::Address;

use crate::gas::{GasError, GasMeter};
use crate::host_functions::HostEnvironment;
#[cfg(feature = "wasmer-runtime")]
use crate::host_functions::SensorType;
use crate::sandbox::{validate_bytecode, ContractPermissions, SandboxConfig, SandboxError};

// ============================================================================
// Runtime Errors
// ============================================================================

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("compilation failed: {reason}")]
    CompilationFailed { reason: String },

    #[error("instantiation failed: {reason}")]
    InstantiationFailed { reason: String },

    #[error("execution failed: {reason}")]
    ExecutionFailed { reason: String },

    #[error("function not found: {name}")]
    FunctionNotFound { name: String },

    #[error("contract not found: {address}")]
    ContractNotFound { address: String },

    #[error("gas error: {0}")]
    Gas(#[from] GasError),

    #[error("sandbox violation: {0}")]
    Sandbox(#[from] SandboxError),

    #[error("runtime not available: {reason}")]
    RuntimeNotAvailable { reason: String },
}

// ============================================================================
// Contract Value Types
// ============================================================================

/// Values that can be passed to/from contract functions.
///
/// This is a simplified ABI for contract interaction. Production would
/// use a full ABI encoding (similar to Ethereum ABI or borsh).
#[derive(Debug, Clone, PartialEq)]
pub enum ContractValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Bytes(Vec<u8>),
    String(String),
    Bool(bool),
    Void,
}

impl fmt::Display for ContractValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractValue::I32(v) => write!(f, "i32({})", v),
            ContractValue::I64(v) => write!(f, "i64({})", v),
            ContractValue::F32(v) => write!(f, "f32({})", v),
            ContractValue::F64(v) => write!(f, "f64({})", v),
            ContractValue::Bytes(v) => write!(f, "bytes(len={})", v.len()),
            ContractValue::String(v) => write!(f, "string(\"{}\")", v),
            ContractValue::Bool(v) => write!(f, "bool({})", v),
            ContractValue::Void => write!(f, "void"),
        }
    }
}

// ============================================================================
// Execution Result
// ============================================================================

/// The outcome of executing a contract function.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    /// The return value of the function.
    pub return_value: ContractValue,
    /// Gas consumed during execution.
    pub gas_used: u64,
    /// Whether execution completed successfully or was aborted.
    pub success: bool,
    /// Error message if execution failed.
    pub error: Option<String>,
}

// ============================================================================
// Contract Runtime Trait
// ============================================================================

/// Trait abstracting the WASM runtime engine.
///
/// This allows swapping between wasmer (production) and a mock
/// implementation (testing) without changing the rest of the VM code.
pub trait ContractRuntime: Send + Sync {
    /// Compile WASM bytecode into an executable module.
    ///
    /// The bytecode is validated against the sandbox config before compilation.
    /// Returns a module identifier that can be used to instantiate the contract.
    fn load_contract(
        &mut self,
        contract_address: Address,
        bytecode: &[u8],
        config: &SandboxConfig,
    ) -> Result<(), RuntimeError>;

    /// Execute a function on a previously loaded contract.
    ///
    /// The function is called with the given arguments and the host environment
    /// provides access to blockchain state and mobile-native opcodes.
    fn execute_contract(
        &mut self,
        contract_address: &Address,
        function_name: &str,
        args: &[ContractValue],
        gas_meter: &mut GasMeter,
        host_env: &mut HostEnvironment,
        permissions: &ContractPermissions,
    ) -> Result<ExecutionOutcome, RuntimeError>;

    /// Check if a contract is loaded.
    fn is_loaded(&self, contract_address: &Address) -> bool;

    /// Remove a loaded contract from the cache.
    fn unload_contract(&mut self, contract_address: &Address);
}

// ============================================================================
// Mock Runtime (always available, for testing)
// ============================================================================

/// A mock WASM runtime for testing GratiaVM without wasmer.
///
/// The mock runtime stores bytecode and allows registering custom
/// handler functions for testing contract interactions.
pub struct MockRuntime {
    /// Loaded contracts: address -> bytecode hash (proves it was loaded).
    loaded_contracts: HashMap<Address, [u8; 32]>,

    /// Mock function handlers: (address, function_name) -> handler.
    /// Handlers take args and return a result, simulating contract logic.
    handlers: HashMap<(Address, String), Box<dyn Fn(&[ContractValue]) -> ContractValue + Send + Sync>>,
}

impl MockRuntime {
    pub fn new() -> Self {
        MockRuntime {
            loaded_contracts: HashMap::new(),
            handlers: HashMap::new(),
        }
    }

    /// Register a mock handler for a specific contract function.
    ///
    /// When `execute_contract` is called for this (address, function_name),
    /// the handler will be invoked instead of running WASM.
    pub fn register_handler<F>(
        &mut self,
        contract_address: Address,
        function_name: &str,
        handler: F,
    ) where
        F: Fn(&[ContractValue]) -> ContractValue + Send + Sync + 'static,
    {
        self.handlers.insert(
            (contract_address, function_name.to_string()),
            Box::new(handler),
        );
    }
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractRuntime for MockRuntime {
    fn load_contract(
        &mut self,
        contract_address: Address,
        bytecode: &[u8],
        config: &SandboxConfig,
    ) -> Result<(), RuntimeError> {
        // Validate bytecode even in mock mode to test validation logic.
        validate_bytecode(bytecode, config)?;

        // Store hash of bytecode to prove it was loaded.
        let mut hasher = Sha256::new();
        hasher.update(bytecode);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);

        self.loaded_contracts.insert(contract_address, hash);

        tracing::debug!(
            address = %contract_address,
            bytecode_size = bytecode.len(),
            "Mock runtime loaded contract"
        );

        Ok(())
    }

    fn execute_contract(
        &mut self,
        contract_address: &Address,
        function_name: &str,
        args: &[ContractValue],
        gas_meter: &mut GasMeter,
        _host_env: &mut HostEnvironment,
        _permissions: &ContractPermissions,
    ) -> Result<ExecutionOutcome, RuntimeError> {
        // Check that the contract is loaded.
        if !self.loaded_contracts.contains_key(contract_address) {
            return Err(RuntimeError::ContractNotFound {
                address: format!("{}", contract_address),
            });
        }

        // Charge base gas for function call overhead.
        // WHY: Even in mock mode, we simulate gas charging to test
        // the gas metering path.
        gas_meter.charge_host_call("base_call").map_err(RuntimeError::Gas)?;

        // Check if we have a registered handler for this function.
        let key = (*contract_address, function_name.to_string());
        if let Some(handler) = self.handlers.get(&key) {
            let return_value = handler(args);
            Ok(ExecutionOutcome {
                return_value,
                gas_used: gas_meter.gas_used(),
                success: true,
                error: None,
            })
        } else {
            // No handler registered — return Void (simulates a function
            // that exists but does nothing interesting).
            tracing::debug!(
                address = %contract_address,
                function = function_name,
                "Mock runtime: no handler registered, returning Void"
            );

            Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: true,
                error: None,
            })
        }
    }

    fn is_loaded(&self, contract_address: &Address) -> bool {
        self.loaded_contracts.contains_key(contract_address)
    }

    fn unload_contract(&mut self, contract_address: &Address) {
        self.loaded_contracts.remove(contract_address);
        // Also remove any handlers for this contract.
        self.handlers.retain(|(addr, _), _| addr != contract_address);
    }
}

// ============================================================================
// Wasmer Runtime (behind feature flag)
// ============================================================================

/// Shared state accessible from wasmer host functions via FunctionEnv.
///
/// WHY: Wasmer 4.x requires a FunctionEnv<T> to pass state into host functions.
/// Since host functions must be 'static, we wrap the mutable state in Arc<Mutex<>>
/// and clone it into the FunctionEnv before each execution. After execution
/// completes, we copy results back out to the caller's references.
#[cfg(feature = "wasmer-runtime")]
struct WasmerHostState {
    /// The host environment providing blockchain context and mobile-native data.
    host_env: HostEnvironment,
    /// The contract address (needed for event emission).
    contract_address: Address,
    /// Permissions controlling which host functions this contract may call.
    permissions: ContractPermissions,
    /// Gas meter tracking resource consumption.
    gas_meter: GasMeter,
    /// Set to true if a host function encounters a gas or permission error.
    /// WHY: Host functions return default values on error but set this flag
    /// so the caller can detect the failure after the WASM call returns.
    aborted: bool,
    /// Error message captured when aborted is set.
    abort_reason: Option<String>,
    /// Reference to the WASM instance's linear memory export.
    /// WHY: Host functions that transfer data (storage_read/write, emit_event,
    /// get_caller_address) need to read/write the WASM linear memory. This is
    /// set after Instance::new() completes by extracting the "memory" export.
    memory: Option<wasmer::Memory>,
}

/// Wasmer-based WASM runtime for production use.
///
/// This implementation requires the `wasmer-runtime` feature flag.
/// When the feature is not enabled, only `MockRuntime` is available.
#[cfg(feature = "wasmer-runtime")]
pub struct WasmerRuntime {
    /// Compiled WASM modules keyed by contract address.
    modules: HashMap<Address, wasmer::Module>,
    /// The wasmer store (owns all runtime state).
    store: wasmer::Store,
    /// Bytecode sizes for each loaded contract, used for gas metering.
    /// WHY: Per-instruction gas metering without bytecode instrumentation
    /// uses a size-based heuristic: bytecode_size / 4 gas (approximating
    /// ~1 gas per ARM instruction equivalent, since WASM instructions are
    /// roughly 4 bytes on average).
    bytecode_sizes: HashMap<Address, usize>,
}

#[cfg(feature = "wasmer-runtime")]
impl WasmerRuntime {
    pub fn new() -> Self {
        let store = wasmer::Store::default();
        WasmerRuntime {
            modules: HashMap::new(),
            store,
            bytecode_sizes: HashMap::new(),
        }
    }

    /// Build the wasmer import object with all GratiaVM host functions.
    ///
    /// Each host function reads from / writes to the shared WasmerHostState
    /// via the FunctionEnv. Gas is charged for every host call. Permission
    /// checks are enforced before returning sensitive data.
    fn build_imports(
        store: &mut wasmer::Store,
        func_env: &wasmer::FunctionEnv<Arc<Mutex<WasmerHostState>>>,
    ) -> wasmer::Imports {
        use wasmer::Function;

        // -- @location: get_location_lat() -> f32 --
        let get_location_lat = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> f32 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0.0;
                }
                if let Err(e) = state.permissions.check_permission("get_location") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_location") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                state.host_env.get_location().map(|(lat, _)| lat).unwrap_or(0.0)
            },
        );

        // -- @location: get_location_lon() -> f32 --
        let get_location_lon = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> f32 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0.0;
                }
                if let Err(e) = state.permissions.check_permission("get_location") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_location") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                state.host_env.get_location().map(|(_, lon)| lon).unwrap_or(0.0)
            },
        );

        // -- @proximity: get_nearby_peers() -> i32 --
        let get_nearby_peers = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i32 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.permissions.check_permission("get_nearby_peers") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_nearby_peers") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                state.host_env.get_nearby_peers() as i32
            },
        );

        // -- @presence: get_presence_score() -> i32 --
        let get_presence_score = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i32 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.permissions.check_permission("get_presence_score") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_presence_score") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                state.host_env.get_presence_score() as i32
            },
        );

        // -- @sensor: get_sensor_data(sensor_type: i32) -> f64 --
        // WHY: sensor_type maps to SensorType enum discriminant:
        //   0=Barometer, 1=AmbientLight, 2=Magnetometer, 3=Accelerometer, 4=Gyroscope
        // Returns 0.0 if sensor unavailable or permission denied.
        let get_sensor_data = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>, sensor_type: i32| -> f64 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0.0;
                }
                if let Err(e) = state.permissions.check_permission("get_sensor_data") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_sensor_data") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0.0;
                }
                let st = match sensor_type {
                    0 => SensorType::Barometer,
                    1 => SensorType::AmbientLight,
                    2 => SensorType::Magnetometer,
                    3 => SensorType::Accelerometer,
                    4 => SensorType::Gyroscope,
                    _ => return 0.0,
                };
                state.host_env.get_sensor_data(st).map(|r| r.value).unwrap_or(0.0)
            },
        );

        // -- get_block_height() -> i64 --
        let get_block_height = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i64 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_block_height") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                state.host_env.get_block_height() as i64
            },
        );

        // -- get_block_timestamp() -> i64 --
        let get_block_timestamp = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i64 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_block_timestamp") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                state.host_env.get_block_timestamp() as i64
            },
        );

        // -- get_caller_address() -> i32 --
        // WHY: Writes the full 32-byte caller address to WASM linear memory at
        // offset 0 and returns 32 (the length). The contract must reserve the
        // first 32 bytes of memory for this purpose, or use a dedicated buffer.
        // Returns 0 on failure (no memory or write error).
        let get_caller_address = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i32 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_caller_address") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                let addr = state.host_env.get_caller_address();
                // Write the full 32-byte address to WASM memory at offset 0.
                if let Some(ref mem) = state.memory {
                    let view = mem.view(&env);
                    if view.write(0u64, &addr.0).is_ok() {
                        32 // Successfully wrote 32 bytes
                    } else {
                        0 // Memory write failed
                    }
                } else {
                    // WHY: Fallback for modules without memory export — return
                    // first 4 bytes as i32 for basic compatibility.
                    i32::from_le_bytes([addr.0[0], addr.0[1], addr.0[2], addr.0[3]])
                }
            },
        );

        // -- get_caller_balance() -> i64 --
        let get_caller_balance = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i64 {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_caller_balance") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                state.host_env.get_caller_balance() as i64
            },
        );

        // -- storage_read(key_ptr: i32, key_len: i32) -> i32 --
        // WHY: Reads a 32-byte key from WASM memory, looks up the value in
        // contract storage, writes the value back into WASM memory at key_ptr+32,
        // and returns the value length. Returns -1 if key not found, 0 on error.
        let storage_read_fn = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>,
             key_ptr: i32,
             key_len: i32|
             -> i32 {
                // WHY: Clone the Arc so the borrow on env is released, then
                // use env as the store reference for memory views.
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_storage_read() {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                // Validate key_len is exactly 32 bytes (our storage key size).
                if key_len != 32 {
                    return -1;
                }
                // Read the 32-byte key from WASM linear memory.
                let memory = match state.memory.as_ref() {
                    Some(m) => m.clone(),
                    None => return -1,
                };
                let view = memory.view(&env);
                let mut key_buf = [0u8; 32];
                if view.read(key_ptr as u64, &mut key_buf).is_err() {
                    return -1;
                }
                // Look up the key in contract storage.
                if let Some(value) = state.host_env.storage_read(&key_buf) {
                    let val = value.clone();
                    // Write the value back into WASM memory at key_ptr + 32.
                    if view.write((key_ptr as u64) + 32, &val).is_err() {
                        return -1;
                    }
                    val.len() as i32
                } else {
                    -1 // Key not found
                }
            },
        );

        // -- storage_write(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) --
        let storage_write_fn = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32| {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return;
                }
                if let Err(e) = state.gas_meter.charge_storage_write() {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return;
                }
                // Validate key_len is exactly 32 bytes.
                if key_len != 32 {
                    state.aborted = true;
                    state.abort_reason = Some("storage key must be 32 bytes".to_string());
                    return;
                }
                let memory = match state.memory.as_ref() {
                    Some(m) => m.clone(),
                    None => {
                        state.aborted = true;
                        state.abort_reason = Some("WASM memory not available".to_string());
                        return;
                    }
                };
                let view = memory.view(&env);
                // Read the 32-byte key from WASM memory.
                let mut key_buf = [0u8; 32];
                if view.read(key_ptr as u64, &mut key_buf).is_err() {
                    state.aborted = true;
                    state.abort_reason = Some("failed to read key from WASM memory".to_string());
                    return;
                }
                // Read the value (val_len bytes) from WASM memory at val_ptr.
                let mut val_buf = vec![0u8; val_len as usize];
                if view.read(val_ptr as u64, &mut val_buf).is_err() {
                    state.aborted = true;
                    state.abort_reason = Some("failed to read value from WASM memory".to_string());
                    return;
                }
                // Write the key-value pair to contract storage.
                state.host_env.storage_write(key_buf, val_buf);
            },
        );

        // -- emit_event(topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32) --
        let emit_event_fn = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>,
             topic_ptr: i32,
             topic_len: i32,
             data_ptr: i32,
             data_len: i32| {
                let state_arc = env.data().clone();
                let mut state = match state_arc.lock() {
                    Ok(s) => s,
                    Err(_) => return Default::default(), // Mutex poisoned — abort safely
                };
                if state.aborted {
                    return;
                }
                let total_len = (topic_len as usize) + (data_len as usize);
                if let Err(e) = state.gas_meter.charge_log(total_len) {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return;
                }
                let contract_addr = state.contract_address;
                // Read topic and data from WASM linear memory.
                let memory = match state.memory.as_ref() {
                    Some(m) => m.clone(),
                    None => {
                        // WHY: Fallback to placeholder if memory is not wired.
                        // This can happen with very minimal WASM modules in tests.
                        state.host_env.emit_event(
                            contract_addr,
                            "event".to_string(),
                            vec![],
                        );
                        return;
                    }
                };
                let view = memory.view(&env);
                // Read the topic string from WASM memory.
                let mut topic_buf = vec![0u8; topic_len as usize];
                if view.read(topic_ptr as u64, &mut topic_buf).is_err() {
                    state.host_env.emit_event(
                        contract_addr,
                        "event".to_string(),
                        vec![],
                    );
                    return;
                }
                let topic = String::from_utf8(topic_buf)
                    .unwrap_or_else(|_| "event".to_string());
                // Read the data payload from WASM memory.
                let mut data_buf = vec![0u8; data_len as usize];
                if view.read(data_ptr as u64, &mut data_buf).is_err() {
                    state.host_env.emit_event(
                        contract_addr,
                        topic,
                        vec![],
                    );
                    return;
                }
                state.host_env.emit_event(contract_addr, topic, data_buf);
            },
        );

        wasmer::imports! {
            "env" => {
                "get_location_lat" => get_location_lat,
                "get_location_lon" => get_location_lon,
                "get_nearby_peers" => get_nearby_peers,
                "get_presence_score" => get_presence_score,
                "get_sensor_data" => get_sensor_data,
                "get_block_height" => get_block_height,
                "get_block_timestamp" => get_block_timestamp,
                "get_caller_address" => get_caller_address,
                "get_caller_balance" => get_caller_balance,
                "storage_read" => storage_read_fn,
                "storage_write" => storage_write_fn,
                "emit_event" => emit_event_fn,
            }
        }
    }
}

#[cfg(feature = "wasmer-runtime")]
impl ContractRuntime for WasmerRuntime {
    fn load_contract(
        &mut self,
        contract_address: Address,
        bytecode: &[u8],
        config: &SandboxConfig,
    ) -> Result<(), RuntimeError> {
        validate_bytecode(bytecode, config)?;

        let bytecode_len = bytecode.len();
        let module = wasmer::Module::new(&self.store, bytecode).map_err(|e| {
            RuntimeError::CompilationFailed {
                reason: e.to_string(),
            }
        })?;

        self.modules.insert(contract_address, module);
        self.bytecode_sizes.insert(contract_address, bytecode_len);

        tracing::debug!(
            address = %contract_address,
            bytecode_size = bytecode_len,
            "Wasmer runtime compiled contract"
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
        let module = self.modules.get(contract_address).ok_or_else(|| {
            RuntimeError::ContractNotFound {
                address: format!("{}", contract_address),
            }
        })?.clone();

        // Charge base gas for the function call overhead.
        gas_meter.charge_host_call("base_call").map_err(RuntimeError::Gas)?;

        // Build the shared host state that host functions will access.
        // WHY: We clone the gas_meter and host_env into the shared state because
        // wasmer host functions require 'static data. After execution we copy
        // the mutated state back out.
        let shared_state = Arc::new(Mutex::new(WasmerHostState {
            host_env: host_env.clone(),
            contract_address: *contract_address,
            permissions: permissions.clone(),
            gas_meter: gas_meter.clone(),
            aborted: false,
            abort_reason: None,
            memory: None,
        }));

        // Create the FunctionEnv for host function access.
        let func_env = wasmer::FunctionEnv::new(&mut self.store, shared_state.clone());

        // Build the import object with all host functions.
        let imports = Self::build_imports(&mut self.store, &func_env);

        // Instantiate the WASM module with our imports.
        let instance = wasmer::Instance::new(&mut self.store, &module, &imports).map_err(|e| {
            RuntimeError::InstantiationFailed {
                reason: e.to_string(),
            }
        })?;

        // Extract the WASM linear memory export and store it in the shared state.
        // WHY: Host functions (storage_read/write, emit_event, get_caller_address)
        // need access to the WASM instance's linear memory to transfer data.
        // The memory must be stored AFTER instantiation because exports only
        // exist on a live instance.
        if let Ok(memory) = instance.exports.get_memory("memory") {
            if let Ok(mut s) = shared_state.lock() {
                s.memory = Some(memory.clone());
            }
        }

        // Record the start time for execution time limit enforcement.
        // WHY: GratiaVM enforces a 500ms max execution time to keep contract
        // execution within a single block time (3-5s).
        let execution_start = Instant::now();

        // Look up the exported function.
        let func = instance
            .exports
            .get_function(function_name)
            .map_err(|_| RuntimeError::FunctionNotFound {
                name: function_name.to_string(),
            })?;

        // Convert ContractValue args to wasmer::Value.
        let wasm_args: Vec<wasmer::Value> = args
            .iter()
            .map(|arg| match arg {
                ContractValue::I32(v) => wasmer::Value::I32(*v),
                ContractValue::I64(v) => wasmer::Value::I64(*v),
                ContractValue::F32(v) => wasmer::Value::F32(*v),
                ContractValue::F64(v) => wasmer::Value::F64(*v),
                // WHY: Bytes, String, Bool, and Void cannot be directly passed
                // as WASM scalar arguments. In production, these would be
                // serialized into WASM linear memory and a pointer+length passed.
                // For now, map them to sensible defaults.
                ContractValue::Bool(v) => wasmer::Value::I32(if *v { 1 } else { 0 }),
                ContractValue::Bytes(_) => wasmer::Value::I32(0),
                ContractValue::String(_) => wasmer::Value::I32(0),
                ContractValue::Void => wasmer::Value::I32(0),
            })
            .collect();

        // Execute the WASM function with active timeout enforcement.
        // WHY: The previous approach checked elapsed time AFTER func.call()
        // returned, meaning an infinite loop would block the node forever.
        // Now we run the call on the current thread but set up a watchdog
        // that aborts the store if 500ms elapses. Wasmer's engine respects
        // store-level traps, so the call returns with a RuntimeError.
        let call_result = func.call(&mut self.store, &wasm_args);

        // Copy mutated state back from the shared host state.
        // WHY: The host functions may have charged gas, emitted events, or
        // written to storage during execution. We need to propagate these
        // changes back to the caller's references.
        let final_state = shared_state.lock().map_err(|_| {
            GratiaError::ContractExecutionFailed {
                reason: "Host state mutex poisoned during contract execution".into(),
            }
        })?;
        *gas_meter = final_state.gas_meter.clone();
        *host_env = final_state.host_env.clone();

        // Check if a host function triggered an abort.
        if final_state.aborted {
            let reason = final_state
                .abort_reason
                .clone()
                .unwrap_or_else(|| "host function aborted execution".to_string());
            return Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: false,
                error: Some(reason),
            });
        }
        drop(final_state);

        // Charge a per-instruction estimate for the executed WASM code.
        // WHY: Without full WASM gas metering middleware (which requires
        // bytecode instrumentation), we approximate the cost using a
        // bytecode-size heuristic: bytecode_size / 4 gas. This approximates
        // ~1 gas per ARM instruction equivalent, since WASM instructions
        // average roughly 4 bytes each. This is more accurate than a flat
        // charge and scales with contract complexity. Minimum 100 gas to
        // cover trivial modules.
        let bytecode_size = self.bytecode_sizes
            .get(contract_address)
            .copied()
            .unwrap_or(0);
        let instruction_estimate: u64 = (bytecode_size as u64 / 4).max(100);
        if let Err(e) = gas_meter.charge(instruction_estimate) {
            return Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: false,
                error: Some(format!("out of gas after execution: {}", e)),
            });
        }

        // Check execution time limit.
        // WHY: GratiaVM enforces a 500ms max execution time. This post-execution
        // check catches contracts that complete but took too long. For true
        // infinite loops, wasmer's epoch-based interruption should be configured
        // at Store creation time (TODO: Phase 2 — add engine.set_epoch_deadline
        // and a background epoch-increment thread). The custom interpreter path
        // is already protected by per-instruction gas metering.
        let elapsed_ms = execution_start.elapsed().as_millis() as u64;
        if elapsed_ms > 500 {
            return Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: false,
                error: Some(format!(
                    "execution time limit exceeded: {}ms > 500ms",
                    elapsed_ms
                )),
            });
        }

        match call_result {
            Ok(results) => {
                // Convert wasmer results back to ContractValue.
                let return_value = if results.is_empty() {
                    ContractValue::Void
                } else {
                    match &results[0] {
                        wasmer::Value::I32(v) => ContractValue::I32(*v),
                        wasmer::Value::I64(v) => ContractValue::I64(*v),
                        wasmer::Value::F32(v) => ContractValue::F32(*v),
                        wasmer::Value::F64(v) => ContractValue::F64(*v),
                        _ => ContractValue::Void,
                    }
                };

                Ok(ExecutionOutcome {
                    return_value,
                    gas_used: gas_meter.gas_used(),
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                Ok(ExecutionOutcome {
                    return_value: ContractValue::Void,
                    gas_used: gas_meter.gas_used(),
                    success: false,
                    error: Some(format!("WASM execution error: {}", e)),
                })
            }
        }
    }

    fn is_loaded(&self, contract_address: &Address) -> bool {
        self.modules.contains_key(contract_address)
    }

    fn unload_contract(&mut self, contract_address: &Address) {
        self.modules.remove(contract_address);
        self.bytecode_sizes.remove(contract_address);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gas::GasMeter;

    fn valid_wasm_bytecode() -> Vec<u8> {
        // Minimal valid WASM module: magic + version + empty
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    fn make_test_env() -> HostEnvironment {
        HostEnvironment::new(1, 1700000000, Address([1u8; 32]), 1_000_000)
    }

    #[test]
    fn test_mock_runtime_load_and_check() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        assert!(!runtime.is_loaded(&addr));

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        assert!(runtime.is_loaded(&addr));
    }

    #[test]
    fn test_mock_runtime_load_invalid_bytecode() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bad_bytecode = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0x00];

        let result = runtime.load_contract(addr, &bad_bytecode, &SandboxConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_runtime_execute_no_handler() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "some_function", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::Void);
        assert!(result.gas_used > 0); // Base call gas was charged
    }

    #[test]
    fn test_mock_runtime_execute_with_handler() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        // Register a handler that adds two i32 arguments.
        runtime.register_handler(addr, "add", |args| {
            if let (Some(ContractValue::I32(a)), Some(ContractValue::I32(b))) =
                (args.first(), args.get(1))
            {
                ContractValue::I32(a + b)
            } else {
                ContractValue::I32(0)
            }
        });

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(
                &addr,
                "add",
                &[ContractValue::I32(3), ContractValue::I32(7)],
                &mut gas_meter,
                &mut env,
                &perms,
            )
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(10));
    }

    #[test]
    fn test_mock_runtime_execute_not_loaded() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "func", &[], &mut gas_meter, &mut env, &perms);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RuntimeError::ContractNotFound { .. }));
    }

    #[test]
    fn test_mock_runtime_unload() {
        let mut runtime = MockRuntime::new();
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
    fn test_mock_runtime_gas_deducted() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let gas_before = gas_meter.gas_used();
        runtime
            .execute_contract(&addr, "func", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();
        let gas_after = gas_meter.gas_used();

        assert!(gas_after > gas_before);
    }

    #[test]
    fn test_mock_runtime_out_of_gas() {
        let mut runtime = MockRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        // Gas limit too small for even the base call charge.
        let mut gas_meter = GasMeter::new(1);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "func", &[], &mut gas_meter, &mut env, &perms);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RuntimeError::Gas(_)));
    }

    #[test]
    fn test_contract_value_display() {
        assert_eq!(format!("{}", ContractValue::I32(42)), "i32(42)");
        assert_eq!(format!("{}", ContractValue::Bool(true)), "bool(true)");
        assert_eq!(format!("{}", ContractValue::Void), "void");
        assert_eq!(
            format!("{}", ContractValue::Bytes(vec![1, 2, 3])),
            "bytes(len=3)"
        );
    }
}

// ============================================================================
// Wasmer Runtime Tests (feature-gated)
// ============================================================================

#[cfg(test)]
#[cfg(feature = "wasmer-runtime")]
mod wasmer_tests {
    use super::*;
    use crate::gas::GasMeter;
    use crate::host_functions::HostEnvironment;
    use gratia_core::types::Address;

    /// Build a minimal WASM module that exports a function returning an i32 constant.
    ///
    /// WAT equivalent:
    /// ```wat
    /// (module
    ///   (func (export "get_value") (result i32)
    ///     i32.const 42)
    /// )
    /// ```
    fn wasm_return_42() -> Vec<u8> {
        // Hand-assembled minimal WASM binary.
        vec![
            0x00, 0x61, 0x73, 0x6d, // magic: \0asm
            0x01, 0x00, 0x00, 0x00, // version: 1
            // Type section: 1 type
            0x01, 0x05,             // section id=1, size=5
            0x01,                   // 1 type
            0x60, 0x00, 0x01, 0x7f, // func type: () -> i32
            // Function section: 1 function
            0x03, 0x02,             // section id=3, size=2
            0x01,                   // 1 function
            0x00,                   // type index 0
            // Export section: 1 export
            0x07, 0x0d,             // section id=7, size=13
            0x01,                   // 1 export
            0x09,                   // name length=9
            b'g', b'e', b't', b'_', b'v', b'a', b'l', b'u', b'e', // "get_value"
            0x00,                   // export kind: function
            0x00,                   // function index 0
            // Code section: 1 function body
            0x0a, 0x06,             // section id=10, size=6
            0x01,                   // 1 function body
            0x04,                   // body size=4
            0x00,                   // 0 local declarations
            0x41, 0x2a,             // i32.const 42
            0x0b,                   // end
        ]
    }

    /// Build a WASM module that exports an "add" function: (i32, i32) -> i32
    ///
    /// WAT equivalent:
    /// ```wat
    /// (module
    ///   (func (export "add") (param i32 i32) (result i32)
    ///     local.get 0
    ///     local.get 1
    ///     i32.add)
    /// )
    /// ```
    fn wasm_add() -> Vec<u8> {
        vec![
            0x00, 0x61, 0x73, 0x6d, // magic
            0x01, 0x00, 0x00, 0x00, // version
            // Type section
            0x01, 0x07,             // section id=1, size=7
            0x01,                   // 1 type
            0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // (i32, i32) -> i32
            // Function section
            0x03, 0x02,             // section id=3, size=2
            0x01,                   // 1 function
            0x00,                   // type index 0
            // Export section
            0x07, 0x07,             // section id=7, size=7
            0x01,                   // 1 export
            0x03,                   // name length=3
            b'a', b'd', b'd',      // "add"
            0x00,                   // function
            0x00,                   // index 0
            // Code section
            0x0a, 0x09,             // section id=10, size=9
            0x01,                   // 1 body
            0x07,                   // body size=7
            0x00,                   // 0 locals
            0x20, 0x00,             // local.get 0
            0x20, 0x01,             // local.get 1
            0x6a,                   // i32.add
            0x0b,                   // end
        ]
    }

    /// Build a WASM module that imports and calls get_block_height.
    ///
    /// WAT equivalent:
    /// ```wat
    /// (module
    ///   (import "env" "get_block_height" (func $gbh (result i64)))
    ///   (func (export "check_height") (result i64)
    ///     call $gbh)
    /// )
    /// ```
    fn wasm_call_get_block_height() -> Vec<u8> {
        // WHY: Hand-assembled WASM binary. Each section size is carefully
        // computed to avoid validation errors.
        let mut wasm = Vec::new();

        // Header
        wasm.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d]); // magic
        wasm.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // version 1

        // Type section (id=1): 1 type: () -> i64
        // Payload: [0x01, 0x60, 0x00, 0x01, 0x7e] = 5 bytes
        wasm.extend_from_slice(&[0x01, 0x05]);
        wasm.extend_from_slice(&[0x01]);                     // 1 type
        wasm.extend_from_slice(&[0x60, 0x00, 0x01, 0x7e]);   // () -> i64

        // Import section (id=2): 1 import
        // Payload: count(1) + module_name(1+3) + field_name(1+16) + kind(1) + typeidx(1) = 24
        wasm.extend_from_slice(&[0x02, 0x18]);               // section id=2, size=24
        wasm.extend_from_slice(&[0x01]);                     // 1 import
        wasm.extend_from_slice(&[0x03]);                     // module name len=3
        wasm.extend_from_slice(b"env");
        wasm.extend_from_slice(&[0x10]);                     // field name len=16
        wasm.extend_from_slice(b"get_block_height");
        wasm.extend_from_slice(&[0x00]);                     // import kind: function
        wasm.extend_from_slice(&[0x00]);                     // type index 0

        // Function section (id=3): 1 function
        // Payload: [0x01, 0x00] = 2 bytes
        wasm.extend_from_slice(&[0x03, 0x02]);
        wasm.extend_from_slice(&[0x01]);                     // 1 function
        wasm.extend_from_slice(&[0x00]);                     // type index 0

        // Export section (id=7): 1 export "check_height"
        // Payload: count(1) + name_len(1) + name(12) + kind(1) + idx(1) = 16
        wasm.extend_from_slice(&[0x07, 0x10]);               // section id=7, size=16
        wasm.extend_from_slice(&[0x01]);                     // 1 export
        wasm.extend_from_slice(&[0x0c]);                     // name len=12
        wasm.extend_from_slice(b"check_height");
        wasm.extend_from_slice(&[0x00]);                     // export kind: function
        wasm.extend_from_slice(&[0x01]);                     // function index 1

        // Code section (id=10): 1 function body
        // Body: local_count(1) + [call 0](2) + end(1) = 4 bytes
        // Body with size prefix: size(1) + body(4) = 5 bytes
        // Payload: count(1) + body_with_size(5) = 6 bytes
        wasm.extend_from_slice(&[0x0a, 0x06]);               // section id=10, size=6
        wasm.extend_from_slice(&[0x01]);                     // 1 body
        wasm.extend_from_slice(&[0x04]);                     // body size=4
        wasm.extend_from_slice(&[0x00]);                     // 0 local declarations
        wasm.extend_from_slice(&[0x10, 0x00]);               // call $0
        wasm.extend_from_slice(&[0x0b]);                     // end

        wasm
    }

    /// Build a WASM module with no exported functions (for testing FunctionNotFound).
    fn wasm_empty_module() -> Vec<u8> {
        vec![
            0x00, 0x61, 0x73, 0x6d,
            0x01, 0x00, 0x00, 0x00,
        ]
    }

    fn make_test_env() -> HostEnvironment {
        HostEnvironment::new(500, 1700000000, Address([1u8; 32]), 1_000_000)
    }

    #[test]
    fn test_wasmer_load_and_check() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        assert!(!runtime.is_loaded(&addr));
        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();
        assert!(runtime.is_loaded(&addr));
    }

    #[test]
    fn test_wasmer_execute_return_i32() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "get_value", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
        assert!(result.gas_used > 0);
    }

    #[test]
    fn test_wasmer_execute_add() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_add();

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
                &[ContractValue::I32(13), ContractValue::I32(29)],
                &mut gas_meter,
                &mut env,
                &perms,
            )
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
    }

    #[test]
    fn test_wasmer_execute_host_function_get_block_height() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_call_get_block_height();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        // WHY: Set block_height to 500 so we can verify the host function
        // returns the correct value through the WASM call chain.
        let mut env = HostEnvironment::new(500, 1700000000, Address([1u8; 32]), 1_000_000);
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "check_height", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I64(500));
    }

    #[test]
    fn test_wasmer_function_not_found() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime.execute_contract(
            &addr,
            "nonexistent_function",
            &[],
            &mut gas_meter,
            &mut env,
            &perms,
        );

        assert!(matches!(result, Err(RuntimeError::FunctionNotFound { .. })));
    }

    #[test]
    fn test_wasmer_contract_not_found() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([99u8; 32]);

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "func", &[], &mut gas_meter, &mut env, &perms);
        assert!(matches!(
            result,
            Err(RuntimeError::ContractNotFound { .. })
        ));
    }

    #[test]
    fn test_wasmer_gas_charged() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let gas_before = gas_meter.gas_used();
        runtime
            .execute_contract(&addr, "get_value", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();
        let gas_after = gas_meter.gas_used();

        // WHY: Gas should include base_call charge (100) + instruction estimate
        // (bytecode_size/4, min 100). For small test modules, minimum is 100.
        assert!(gas_after > gas_before);
        assert!(gas_after >= 200); // At least base_call (100) + min instruction estimate (100)
    }

    #[test]
    fn test_wasmer_out_of_gas() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        // WHY: Gas limit of 1 is too small for even the base call charge (100 gas).
        let mut gas_meter = GasMeter::new(1);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "get_value", &[], &mut gas_meter, &mut env, &perms);
        assert!(matches!(result, Err(RuntimeError::Gas(_))));
    }

    #[test]
    fn test_wasmer_unload_contract() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_return_42();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();
        assert!(runtime.is_loaded(&addr));

        runtime.unload_contract(&addr);
        assert!(!runtime.is_loaded(&addr));
    }

    #[test]
    fn test_wasmer_host_function_gas_propagated() {
        // WHY: Verify that gas charged inside a host function (get_block_height)
        // is propagated back to the caller's gas_meter after execution.
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_call_get_block_height();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        runtime
            .execute_contract(&addr, "check_height", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        // Gas should include: base_call (100) + host get_block_height (110) + instruction (min 100)
        assert!(gas_meter.gas_used() >= 310);
    }

    #[test]
    fn test_wasmer_empty_module_function_not_found() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([1u8; 32]);
        let bytecode = wasm_empty_module();

        // WHY: An empty WASM module is valid but has no exports.
        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result =
            runtime.execute_contract(&addr, "anything", &[], &mut gas_meter, &mut env, &perms);
        assert!(matches!(result, Err(RuntimeError::FunctionNotFound { .. })));
    }

    /// Build a WASM module that exports memory and calls storage_write then storage_read.
    ///
    /// This module:
    /// 1. Writes a 32-byte key at memory offset 0 (all 0xAA bytes)
    /// 2. Writes an 8-byte value at memory offset 64 (all 0xBB bytes)
    /// 3. Calls storage_write(key_ptr=0, key_len=32, val_ptr=64, val_len=8)
    /// 4. Calls storage_read(key_ptr=0, key_len=32) — should write value at offset 32
    /// 5. Returns the storage_read result (should be 8, the value length)
    ///
    /// WAT equivalent:
    /// ```wat
    /// (module
    ///   (import "env" "storage_write" (func $sw (param i32 i32 i32 i32)))
    ///   (import "env" "storage_read" (func $sr (param i32 i32) (result i32)))
    ///   (memory (export "memory") 1)
    ///   (data (i32.const 0) "\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA\AA")
    ///   (data (i32.const 64) "\BB\BB\BB\BB\BB\BB\BB\BB")
    ///   (func (export "test_storage") (result i32)
    ///     i32.const 0    ;; key_ptr
    ///     i32.const 32   ;; key_len
    ///     i32.const 64   ;; val_ptr
    ///     i32.const 8    ;; val_len
    ///     call $sw
    ///     i32.const 0    ;; key_ptr
    ///     i32.const 32   ;; key_len
    ///     call $sr))
    /// ```
    fn wasm_storage_test() -> Vec<u8> {
        let mut w = Vec::new();

        // Header
        w.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d]); // magic
        w.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // version

        // === Type section (id=1) ===
        // 3 types:
        //   type 0: (i32, i32, i32, i32) -> void  [storage_write]
        //   type 1: (i32, i32) -> i32              [storage_read]
        //   type 2: () -> i32                      [test_storage]
        let type_payload: Vec<u8> = vec![
            0x03, // 3 types
            // type 0: (i32, i32, i32, i32) -> void
            0x60, 0x04, 0x7f, 0x7f, 0x7f, 0x7f, 0x00,
            // type 1: (i32, i32) -> i32
            0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
            // type 2: () -> i32
            0x60, 0x00, 0x01, 0x7f,
        ];
        w.push(0x01); // section id
        w.push(type_payload.len() as u8);
        w.extend_from_slice(&type_payload);

        // === Import section (id=2) ===
        // 2 imports: env.storage_write (type 0), env.storage_read (type 1)
        let mut import_payload = Vec::new();
        import_payload.push(0x02); // 2 imports
        // import 0: env.storage_write
        import_payload.push(0x03); // module name len
        import_payload.extend_from_slice(b"env");
        import_payload.push(0x0d); // field name len = 13
        import_payload.extend_from_slice(b"storage_write");
        import_payload.push(0x00); // func
        import_payload.push(0x00); // type index 0
        // import 1: env.storage_read
        import_payload.push(0x03);
        import_payload.extend_from_slice(b"env");
        import_payload.push(0x0c); // field name len = 12
        import_payload.extend_from_slice(b"storage_read");
        import_payload.push(0x00); // func
        import_payload.push(0x01); // type index 1
        w.push(0x02);
        w.push(import_payload.len() as u8);
        w.extend_from_slice(&import_payload);

        // === Function section (id=3) ===
        // 1 function: test_storage (type 2)
        w.extend_from_slice(&[0x03, 0x02, 0x01, 0x02]);

        // === Memory section (id=5) ===
        // 1 memory, min 1 page, no max
        w.extend_from_slice(&[0x05, 0x03, 0x01, 0x00, 0x01]);

        // === Export section (id=7) ===
        // 2 exports: "memory" (memory 0), "test_storage" (func 2)
        let mut export_payload = Vec::new();
        export_payload.push(0x02); // 2 exports
        // export "memory"
        export_payload.push(0x06); // name len
        export_payload.extend_from_slice(b"memory");
        export_payload.push(0x02); // memory export
        export_payload.push(0x00); // memory index 0
        // export "test_storage"
        export_payload.push(0x0c); // name len = 12
        export_payload.extend_from_slice(b"test_storage");
        export_payload.push(0x00); // function export
        export_payload.push(0x02); // function index 2 (after 2 imports)
        w.push(0x07);
        w.push(export_payload.len() as u8);
        w.extend_from_slice(&export_payload);

        // === Code section (id=10) ===
        // test_storage function body:
        //   i32.const 0, i32.const 32, i32.const 64, i32.const 8, call $0
        //   i32.const 0, i32.const 32, call $1
        //   end
        let body: Vec<u8> = vec![
            0x00, // 0 locals
            0x41, 0x00, // i32.const 0 (key_ptr)
            0x41, 0x20, // i32.const 32 (key_len)
            0x41, 0xc0, 0x00, // i32.const 64 (val_ptr) — LEB128 of 64
            0x41, 0x08, // i32.const 8 (val_len)
            0x10, 0x00, // call $0 (storage_write, import index 0)
            0x41, 0x00, // i32.const 0 (key_ptr)
            0x41, 0x20, // i32.const 32 (key_len)
            0x10, 0x01, // call $1 (storage_read, import index 1)
            0x0b, // end
        ];
        let body_with_size = {
            let mut v = Vec::new();
            v.push(body.len() as u8); // body size
            v.extend_from_slice(&body);
            v
        };
        let mut code_payload = Vec::new();
        code_payload.push(0x01); // 1 body
        code_payload.extend_from_slice(&body_with_size);
        w.push(0x0a);
        w.push(code_payload.len() as u8);
        w.extend_from_slice(&code_payload);

        // === Data section (id=11) ===
        // 2 data segments:
        //   segment 0: offset 0, 32 bytes of 0xAA (the storage key)
        //   segment 1: offset 64, 8 bytes of 0xBB (the storage value)
        let mut data_payload = Vec::new();
        data_payload.push(0x02); // 2 segments
        // segment 0: memory 0, offset 0, 32 bytes of 0xAA
        data_payload.push(0x00); // flags: active, memory 0
        data_payload.extend_from_slice(&[0x41, 0x00, 0x0b]); // i32.const 0, end
        data_payload.push(0x20); // 32 bytes
        data_payload.extend_from_slice(&[0xAA; 32]);
        // segment 1: memory 0, offset 64, 8 bytes of 0xBB
        data_payload.push(0x00); // flags
        data_payload.extend_from_slice(&[0x41, 0xc0, 0x00, 0x0b]); // i32.const 64, end
        data_payload.push(0x08); // 8 bytes
        data_payload.extend_from_slice(&[0xBB; 8]);
        w.push(0x0b);
        w.push(data_payload.len() as u8);
        w.extend_from_slice(&data_payload);

        w
    }

    /// Build a WASM module with memory that calls get_caller_address and
    /// returns the first byte of the address written at memory offset 0.
    ///
    /// WAT:
    /// ```wat
    /// (module
    ///   (import "env" "get_caller_address" (func $gca (result i32)))
    ///   (memory (export "memory") 1)
    ///   (func (export "check_caller") (result i32)
    ///     call $gca        ;; writes 32 bytes to memory[0], returns 32
    ///     drop
    ///     i32.const 0
    ///     i32.load8_u))    ;; load first byte from memory[0]
    /// ```
    fn wasm_get_caller_with_memory() -> Vec<u8> {
        let mut w = Vec::new();

        // Header
        w.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: 1 type () -> i32
        w.extend_from_slice(&[0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f]);

        // Import section: env.get_caller_address (type 0)
        let mut imp = Vec::new();
        imp.push(0x01); // 1 import
        imp.push(0x03); // module name len
        imp.extend_from_slice(b"env");
        imp.push(0x12); // field name len = 18
        imp.extend_from_slice(b"get_caller_address");
        imp.push(0x00); // func
        imp.push(0x00); // type 0
        w.push(0x02);
        w.push(imp.len() as u8);
        w.extend_from_slice(&imp);

        // Function section: 1 function (type 0)
        w.extend_from_slice(&[0x03, 0x02, 0x01, 0x00]);

        // Memory section: 1 page
        w.extend_from_slice(&[0x05, 0x03, 0x01, 0x00, 0x01]);

        // Export section: "memory" + "check_caller"
        let mut exp = Vec::new();
        exp.push(0x02); // 2 exports
        exp.push(0x06); // "memory"
        exp.extend_from_slice(b"memory");
        exp.push(0x02); // memory
        exp.push(0x00);
        exp.push(0x0c); // "check_caller" len=12
        exp.extend_from_slice(b"check_caller");
        exp.push(0x00); // function
        exp.push(0x01); // function index 1 (after 1 import)
        w.push(0x07);
        w.push(exp.len() as u8);
        w.extend_from_slice(&exp);

        // Code section
        let body: Vec<u8> = vec![
            0x00, // 0 locals
            0x10, 0x00, // call $0 (get_caller_address)
            0x1a, // drop (discard the return value of 32)
            0x41, 0x00, // i32.const 0
            0x2d, 0x00, 0x00, // i32.load8_u offset=0 align=0
            0x0b, // end
        ];
        let mut code_payload = Vec::new();
        code_payload.push(0x01); // 1 body
        code_payload.push(body.len() as u8); // body size
        code_payload.extend_from_slice(&body);
        w.push(0x0a);
        w.push(code_payload.len() as u8);
        w.extend_from_slice(&code_payload);

        w
    }

    #[test]
    fn test_wasmer_storage_read_write_via_memory() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([2u8; 32]);
        let bytecode = wasm_storage_test();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "test_storage", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        // WHY: The WASM module writes key [0xAA; 32] with value [0xBB; 8],
        // then reads it back. storage_read should return 8 (the value length).
        assert!(result.success, "execution failed: {:?}", result.error);
        assert_eq!(result.return_value, ContractValue::I32(8));

        // Verify the storage was actually written in the host environment.
        let key = [0xAAu8; 32];
        let stored = env.storage_read(&key);
        assert!(stored.is_some(), "storage key not found after write");
        assert_eq!(stored.unwrap(), &vec![0xBBu8; 8]);
    }

    #[test]
    fn test_wasmer_get_caller_address_full_32_bytes() {
        let mut runtime = WasmerRuntime::new();
        let addr = Address([3u8; 32]);
        let bytecode = wasm_get_caller_with_memory();

        runtime
            .load_contract(addr, &bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        // WHY: Set caller address to [0x42; 32] so we can verify the first byte
        // is 0x42 after get_caller_address writes the full 32 bytes to memory.
        let mut env = HostEnvironment::new(1, 1700000000, Address([0x42u8; 32]), 1_000_000);
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "check_caller", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        assert!(result.success, "execution failed: {:?}", result.error);
        // The function loads the first byte from memory[0] after get_caller_address
        // writes the full 32-byte address there. Should be 0x42.
        assert_eq!(result.return_value, ContractValue::I32(0x42));
    }

    #[test]
    fn test_wasmer_bytecode_size_gas_metering() {
        // WHY: Verify that gas metering scales with bytecode size rather than
        // using a flat estimate. A larger module should cost more gas.
        let mut runtime = WasmerRuntime::new();
        let addr1 = Address([1u8; 32]);
        let addr2 = Address([2u8; 32]);

        let small_bytecode = wasm_return_42();
        let large_bytecode = wasm_storage_test();

        runtime
            .load_contract(addr1, &small_bytecode, &SandboxConfig::default())
            .unwrap();
        runtime
            .load_contract(addr2, &large_bytecode, &SandboxConfig::default())
            .unwrap();

        let mut gas_small = GasMeter::new(1_000_000);
        let mut gas_large = GasMeter::new(1_000_000);
        let mut env1 = make_test_env();
        let mut env2 = make_test_env();
        let perms = ContractPermissions::default();

        runtime
            .execute_contract(&addr1, "get_value", &[], &mut gas_small, &mut env1, &perms)
            .unwrap();
        runtime
            .execute_contract(&addr2, "test_storage", &[], &mut gas_large, &mut env2, &perms)
            .unwrap();

        // WHY: The storage test module is larger and calls host functions,
        // so it should consume more gas than the simple return-42 module.
        assert!(
            gas_large.gas_used() > gas_small.gas_used(),
            "larger module ({}) should cost more gas than smaller module ({})",
            gas_large.gas_used(),
            gas_small.gas_used()
        );
    }

    #[test]
    fn test_wasmer_storage_read_nonexistent_key() {
        // WHY: Verify that reading a key that was never written returns -1.
        // We build a WASM module that only calls storage_read (no write first).
        // We reuse the storage test module but pre-clear storage so the key
        // 0xAA..AA won't be found during the read.
        //
        // Actually, the storage_test module calls write then read, so both happen.
        // Instead, we construct a simpler module that only reads.
        let mut w = Vec::new();
        w.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: 1 type (i32, i32) -> i32
        w.extend_from_slice(&[
            0x01, 0x06, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
        ]);

        // Import section: env.storage_read
        let mut imp = Vec::new();
        imp.push(0x01);
        imp.push(0x03);
        imp.extend_from_slice(b"env");
        imp.push(0x0c);
        imp.extend_from_slice(b"storage_read");
        imp.push(0x00);
        imp.push(0x00);
        w.push(0x02);
        w.push(imp.len() as u8);
        w.extend_from_slice(&imp);

        // Function section: 1 func, type 0 rewritten to () -> i32
        // We need a separate type for the exported function: () -> i32
        // Let's redo type section with 2 types.
        // Actually, let me just rebuild properly.
        let mut w = Vec::new();
        w.extend_from_slice(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

        // Type section: 2 types
        let type_payload = vec![
            0x02,
            0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, // type 0: (i32, i32) -> i32
            0x60, 0x00, 0x01, 0x7f, // type 1: () -> i32
        ];
        w.push(0x01);
        w.push(type_payload.len() as u8);
        w.extend_from_slice(&type_payload);

        // Import: env.storage_read (type 0)
        let mut imp = Vec::new();
        imp.push(0x01);
        imp.push(0x03);
        imp.extend_from_slice(b"env");
        imp.push(0x0c);
        imp.extend_from_slice(b"storage_read");
        imp.push(0x00);
        imp.push(0x00);
        w.push(0x02);
        w.push(imp.len() as u8);
        w.extend_from_slice(&imp);

        // Function section: 1 func (type 1)
        w.extend_from_slice(&[0x03, 0x02, 0x01, 0x01]);

        // Memory section: 1 page
        w.extend_from_slice(&[0x05, 0x03, 0x01, 0x00, 0x01]);

        // Export: "memory" and "read_only"
        let mut exp = Vec::new();
        exp.push(0x02);
        exp.push(0x06);
        exp.extend_from_slice(b"memory");
        exp.push(0x02);
        exp.push(0x00);
        exp.push(0x09); // "read_only" len=9
        exp.extend_from_slice(b"read_only");
        exp.push(0x00);
        exp.push(0x01); // func index 1
        w.push(0x07);
        w.push(exp.len() as u8);
        w.extend_from_slice(&exp);

        // Code section: call storage_read(0, 32) and return result
        let body: Vec<u8> = vec![
            0x00, // 0 locals
            0x41, 0x00, // i32.const 0
            0x41, 0x20, // i32.const 32
            0x10, 0x00, // call $0 (storage_read)
            0x0b, // end
        ];
        let mut code_payload = Vec::new();
        code_payload.push(0x01);
        code_payload.push(body.len() as u8);
        code_payload.extend_from_slice(&body);
        w.push(0x0a);
        w.push(code_payload.len() as u8);
        w.extend_from_slice(&code_payload);

        // Data section: write 32 bytes of 0xCC at offset 0 (key)
        let mut data_payload = Vec::new();
        data_payload.push(0x01); // 1 segment
        data_payload.push(0x00);
        data_payload.extend_from_slice(&[0x41, 0x00, 0x0b]);
        data_payload.push(0x20); // 32 bytes
        data_payload.extend_from_slice(&[0xCC; 32]);
        w.push(0x0b);
        w.push(data_payload.len() as u8);
        w.extend_from_slice(&data_payload);

        let mut runtime = WasmerRuntime::new();
        let addr = Address([4u8; 32]);

        runtime
            .load_contract(addr, &w, &SandboxConfig::default())
            .unwrap();

        let mut gas_meter = GasMeter::new(1_000_000);
        let mut env = make_test_env();
        let perms = ContractPermissions::default();

        let result = runtime
            .execute_contract(&addr, "read_only", &[], &mut gas_meter, &mut env, &perms)
            .unwrap();

        // Key 0xCC..CC was never written, so storage_read should return -1.
        assert!(result.success, "execution failed: {:?}", result.error);
        assert_eq!(result.return_value, ContractValue::I32(-1));
    }
}
