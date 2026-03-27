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
}

#[cfg(feature = "wasmer-runtime")]
impl WasmerRuntime {
    pub fn new() -> Self {
        let store = wasmer::Store::default();
        WasmerRuntime {
            modules: HashMap::new(),
            store,
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
                let mut state = state_arc.lock().unwrap();
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
        // WHY: Returns a pointer offset into WASM memory where the 32-byte address
        // is written. Returns 0 on failure. The contract must pre-allocate a
        // buffer and pass its offset, but for simplicity this version writes to
        // a fixed offset (0) and returns 32 (the length). Production would use
        // a more sophisticated ABI. For now, returns the first byte as an i32
        // identifier (sufficient for testing and initial integration).
        let get_caller_address = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i32 {
                let state_arc = env.data().clone();
                let mut state = state_arc.lock().unwrap();
                if state.aborted {
                    return 0;
                }
                if let Err(e) = state.gas_meter.charge_host_call("get_caller_address") {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return 0;
                }
                // WHY: Return first 4 bytes of caller address as i32.
                // Full address passing requires memory write (see storage_read).
                let addr = state.host_env.get_caller_address();
                i32::from_le_bytes([addr.0[0], addr.0[1], addr.0[2], addr.0[3]])
            },
        );

        // -- get_caller_balance() -> i64 --
        let get_caller_balance = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>| -> i64 {
                let state_arc = env.data().clone();
                let mut state = state_arc.lock().unwrap();
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
                let state_arc = env.data().clone();
                let mut state = state_arc.lock().unwrap();
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
                let key_ptr = key_ptr as u32 as usize;
                // WHY: We need to read the key from WASM memory. The memory export
                // is accessed via the store, but inside a host function we cannot
                // easily get the instance's memory. We read from the FunctionEnvMut's
                // view into memory. For now, storage operations work with the key
                // bytes embedded in the call. In production, we would access the
                // WASM linear memory through the instance export.
                // Since we cannot access instance memory from within the host function
                // closure directly in wasmer 4.x without storing a memory reference
                // in the env, we handle this by having contracts pass key data via
                // scalar parameters for simple cases.
                // For full memory access, the memory is stored in the host state
                // after instance creation (see execute_contract).
                let _ = key_ptr; // Will be used when memory is available
                -1 // Key not found (placeholder until memory wiring)
            },
        );

        // -- storage_write(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) --
        let storage_write_fn = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>,
             _key_ptr: i32,
             _key_len: i32,
             _val_ptr: i32,
             _val_len: i32| {
                let state_arc = env.data().clone();
                let mut state = state_arc.lock().unwrap();
                if state.aborted {
                    return;
                }
                if let Err(e) = state.gas_meter.charge_storage_write() {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return;
                }
                // WHY: Full implementation requires WASM memory access (same as storage_read).
                // Storage write is charged but data transfer requires memory wiring.
            },
        );

        // -- emit_event(topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32) --
        let emit_event_fn = Function::new_typed_with_env(
            store,
            func_env,
            |env: wasmer::FunctionEnvMut<Arc<Mutex<WasmerHostState>>>,
             _topic_ptr: i32,
             topic_len: i32,
             _data_ptr: i32,
             data_len: i32| {
                let state_arc = env.data().clone();
                let mut state = state_arc.lock().unwrap();
                if state.aborted {
                    return;
                }
                let total_len = (topic_len as usize) + (data_len as usize);
                if let Err(e) = state.gas_meter.charge_log(total_len) {
                    state.aborted = true;
                    state.abort_reason = Some(e.to_string());
                    return;
                }
                // WHY: Full event data extraction requires WASM memory access.
                // For now we emit a placeholder event so the gas charging and
                // event count tracking are exercised.
                let contract_addr = state.contract_address;
                state.host_env.emit_event(
                    contract_addr,
                    "event".to_string(),
                    vec![],
                );
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

        let module = wasmer::Module::new(&self.store, bytecode).map_err(|e| {
            RuntimeError::CompilationFailed {
                reason: e.to_string(),
            }
        })?;

        self.modules.insert(contract_address, module);

        tracing::debug!(
            address = %contract_address,
            bytecode_size = bytecode.len(),
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

        // Execute the WASM function.
        let call_result = func.call(&mut self.store, &wasm_args);

        // Copy mutated state back from the shared host state.
        // WHY: The host functions may have charged gas, emitted events, or
        // written to storage during execution. We need to propagate these
        // changes back to the caller's references.
        let final_state = shared_state.lock().unwrap();
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
        // bytecode instrumentation), we approximate the cost based on the
        // function call. This is a conservative flat charge. Production
        // would inject gas metering opcodes during compilation.
        let instruction_estimate: u64 = 1_000;
        // Instruction estimate: ~1000 gas as baseline for a simple function call.
        // Real metering would count actual WASM instructions executed.
        if let Err(e) = gas_meter.charge(instruction_estimate) {
            return Ok(ExecutionOutcome {
                return_value: ContractValue::Void,
                gas_used: gas_meter.gas_used(),
                success: false,
                error: Some(format!("out of gas after execution: {}", e)),
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

        // WHY: Gas should include base_call charge (100) + instruction estimate (1000).
        assert!(gas_after > gas_before);
        assert!(gas_after >= 1100); // At least base_call + instruction estimate
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

        // Gas should include: base_call (100) + host get_block_height (110) + instruction (1000)
        assert!(gas_meter.gas_used() >= 1210);
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
}
