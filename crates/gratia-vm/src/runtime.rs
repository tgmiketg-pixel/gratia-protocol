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

use sha2::{Digest, Sha256};
use thiserror::Error;

use gratia_core::types::Address;

use crate::gas::{GasError, GasMeter};
use crate::host_functions::HostEnvironment;
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
        let _module = self.modules.get(contract_address).ok_or_else(|| {
            RuntimeError::ContractNotFound {
                address: format!("{}", contract_address),
            }
        })?;

        // TODO: Phase 2 implementation —
        // 1. Create wasmer Instance with import object containing host functions
        // 2. Set up memory limits per SandboxConfig
        // 3. Register gas metering middleware
        // 4. Call the exported function
        // 5. Collect return values and events

        // For now, return a placeholder indicating the runtime is available
        // but full execution is not yet wired up.
        Err(RuntimeError::ExecutionFailed {
            reason: "Wasmer execution not yet fully implemented — use MockRuntime for testing".to_string(),
        })
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
