//! gratia-vm — GratiaVM WASM-based smart contract execution engine.
//!
//! This crate implements the virtual machine for executing smart contracts
//! on the Gratia blockchain. Key design principles:
//!
//! - **WASM-based:** Contracts compile to WebAssembly for deterministic,
//!   sandboxed execution across all validator nodes.
//! - **ARM-optimized:** Gas costs are calibrated to ARM compute cycles,
//!   and resource limits are tuned for smartphone hardware.
//! - **Mobile-native opcodes:** Contracts can access GPS location, Bluetooth
//!   proximity, Proof of Life presence scores, and sensor data via host
//!   functions (@location, @proximity, @presence, @sensor).
//! - **Strict sandboxing:** 256 MB memory limit, 500ms execution time,
//!   permission-based access to host functions.
//!
//! # Feature Flags
//!
//! - `wasmer-runtime`: Enables the real wasmer WASM runtime. Without this
//!   flag, only `MockRuntime` is available (sufficient for testing and
//!   development).

pub mod gas;
pub mod host_functions;
pub mod interpreter;
pub mod runtime;
pub mod sandbox;

use sha2::{Digest, Sha256};
use tracing;

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, Lux};

use crate::gas::GasMeter;
use crate::host_functions::{ContractEvent, HostEnvironment};
use crate::runtime::{ContractRuntime, ContractValue, RuntimeError};
use crate::sandbox::{ContractPermissions, SandboxConfig};

// ============================================================================
// Contract Call
// ============================================================================

/// A request to call a smart contract function.
#[derive(Debug, Clone)]
pub struct ContractCall {
    /// Address of the account initiating the call.
    pub caller: Address,
    /// Address of the contract to call.
    pub contract_address: Address,
    /// Name of the function to invoke.
    pub function_name: String,
    /// Encoded arguments to pass to the function.
    pub args: Vec<ContractValue>,
    /// Maximum gas the caller is willing to spend (in Lux).
    pub gas_limit: Lux,
}

// ============================================================================
// Execution Result
// ============================================================================

/// The full result of a contract execution, including gas accounting and events.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Whether the execution completed successfully.
    pub success: bool,
    /// The return value from the contract function.
    pub return_value: ContractValue,
    /// Gas consumed during execution (in Lux).
    pub gas_used: Lux,
    /// Gas remaining from the gas limit.
    pub gas_remaining: Lux,
    /// Events emitted during execution.
    pub events: Vec<ContractEvent>,
    /// Error message if execution failed.
    pub error: Option<String>,
}

// ============================================================================
// Deployed Contract
// ============================================================================

/// Metadata for a deployed contract stored on-chain.
#[derive(Debug, Clone)]
struct DeployedContract {
    /// The contract's address (derived from deployer + nonce or bytecode hash).
    /// Used for contract call routing (Phase 2 full VM).
    #[allow(dead_code)]
    address: Address,
    /// SHA-256 hash of the WASM bytecode.
    bytecode_hash: [u8; 32],
    /// The raw WASM bytecode.
    /// WHY: Stored so the contract can be re-loaded into the runtime after
    /// eviction from the module cache. In production, this would be stored
    /// in the state DB, not in memory.
    bytecode: Vec<u8>,
    /// Permissions declared by the contract at deployment.
    permissions: ContractPermissions,
}

// ============================================================================
// GratiaVm
// ============================================================================

/// The top-level GratiaVM smart contract engine.
///
/// Ties together the WASM runtime, gas metering, sandboxing, and host
/// function environment to provide a complete contract execution pipeline.
pub struct GratiaVm {
    /// The underlying WASM runtime (wasmer or mock).
    runtime: Box<dyn ContractRuntime>,
    /// Sandbox configuration (resource limits).
    sandbox_config: SandboxConfig,
    /// Deployed contracts registry.
    /// WHY: In production, this would be backed by the state DB (gratia-state).
    /// Here we keep an in-memory registry for the VM layer.
    contracts: std::collections::HashMap<Address, DeployedContract>,
}

impl GratiaVm {
    /// Create a new GratiaVm with the given runtime and default sandbox config.
    pub fn new(runtime: Box<dyn ContractRuntime>) -> Self {
        GratiaVm {
            runtime,
            sandbox_config: SandboxConfig::default(),
            contracts: std::collections::HashMap::new(),
        }
    }

    /// Create a new GratiaVm with a custom sandbox configuration.
    pub fn with_config(runtime: Box<dyn ContractRuntime>, sandbox_config: SandboxConfig) -> Self {
        GratiaVm {
            runtime,
            sandbox_config,
            contracts: std::collections::HashMap::new(),
        }
    }

    /// Deploy a new smart contract.
    ///
    /// The bytecode is validated, compiled, and stored. The contract address
    /// is derived from the deployer's address and the bytecode hash.
    ///
    /// Returns the deployed contract's address.
    pub fn deploy_contract(
        &mut self,
        deployer: &Address,
        bytecode: &[u8],
        permissions: ContractPermissions,
    ) -> Result<Address, GratiaError> {
        // Validate bytecode before anything else.
        sandbox::validate_bytecode(bytecode, &self.sandbox_config).map_err(|e| {
            GratiaError::Other(format!("contract bytecode validation failed: {}", e))
        })?;

        // Derive contract address from deployer address + bytecode hash.
        // WHY: This makes the address deterministic — the same deployer
        // deploying the same bytecode always gets the same address.
        // In production, a nonce would be included to allow multiple
        // deployments of the same bytecode.
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-contract-v1:");
        hasher.update(&deployer.0);
        hasher.update(bytecode);
        let result = hasher.finalize();
        let mut addr_bytes = [0u8; 32];
        addr_bytes.copy_from_slice(&result);
        let contract_address = Address(addr_bytes);

        // Compute bytecode hash for integrity verification.
        let mut hasher = Sha256::new();
        hasher.update(bytecode);
        let hash_result = hasher.finalize();
        let mut bytecode_hash = [0u8; 32];
        bytecode_hash.copy_from_slice(&hash_result);

        // Compile the bytecode via the runtime.
        self.runtime
            .load_contract(contract_address, bytecode, &self.sandbox_config)
            .map_err(|e| GratiaError::Other(format!("contract compilation failed: {}", e)))?;

        // Store the deployed contract metadata.
        self.contracts.insert(
            contract_address,
            DeployedContract {
                address: contract_address,
                bytecode_hash,
                bytecode: bytecode.to_vec(),
                permissions,
            },
        );

        tracing::info!(
            deployer = %deployer,
            contract = %contract_address,
            bytecode_size = bytecode.len(),
            "Deployed contract"
        );

        Ok(contract_address)
    }

    /// Call a function on a deployed contract.
    ///
    /// This is the main entry point for contract execution. It:
    /// 1. Looks up the contract
    /// 2. Ensures it is loaded in the runtime (re-loads if evicted)
    /// 3. Sets up gas metering and sandboxing
    /// 4. Executes the function with the host environment
    /// 5. Returns the result with gas accounting and events
    pub fn call_contract(
        &mut self,
        call: &ContractCall,
        host_env: &mut HostEnvironment,
    ) -> Result<ExecutionResult, GratiaError> {
        // Look up the contract.
        let contract = self.contracts.get(&call.contract_address).ok_or_else(|| {
            GratiaError::Other(format!(
                "contract not found: {}",
                call.contract_address
            ))
        })?;
        let permissions = contract.permissions.clone();
        let bytecode = contract.bytecode.clone();

        // Ensure the contract is loaded in the runtime.
        // WHY: The runtime may have evicted the module from its cache
        // (e.g., under memory pressure on mobile). We re-load if needed.
        if !self.runtime.is_loaded(&call.contract_address) {
            self.runtime
                .load_contract(call.contract_address, &bytecode, &self.sandbox_config)
                .map_err(|e| {
                    GratiaError::Other(format!("failed to reload contract: {}", e))
                })?;
        }

        // Set up gas metering.
        let mut gas_meter = GasMeter::new(call.gas_limit);

        // Execute the contract function.
        let outcome = self.runtime.execute_contract(
            &call.contract_address,
            &call.function_name,
            &call.args,
            &mut gas_meter,
            host_env,
            &permissions,
        );

        // Collect events emitted during execution.
        let events = host_env.take_events();

        match outcome {
            Ok(exec_outcome) => {
                tracing::debug!(
                    contract = %call.contract_address,
                    function = %call.function_name,
                    gas_used = exec_outcome.gas_used,
                    success = exec_outcome.success,
                    "Contract call completed"
                );

                Ok(ExecutionResult {
                    success: exec_outcome.success,
                    return_value: exec_outcome.return_value,
                    gas_used: gas_meter.gas_used(),
                    gas_remaining: gas_meter.gas_remaining(),
                    events,
                    error: exec_outcome.error,
                })
            }
            Err(RuntimeError::Gas(gas_err)) => {
                // Out of gas — execution aborted, but this is not an error
                // at the protocol level. The caller simply ran out of gas.
                tracing::debug!(
                    contract = %call.contract_address,
                    function = %call.function_name,
                    "Contract call ran out of gas"
                );

                Ok(ExecutionResult {
                    success: false,
                    return_value: ContractValue::Void,
                    gas_used: gas_meter.gas_used(),
                    gas_remaining: 0,
                    events: vec![], // Events are discarded on failure
                    error: Some(format!("out of gas: {}", gas_err)),
                })
            }
            Err(e) => {
                // Runtime error — something went wrong during execution.
                tracing::warn!(
                    contract = %call.contract_address,
                    function = %call.function_name,
                    error = %e,
                    "Contract call failed"
                );

                Ok(ExecutionResult {
                    success: false,
                    return_value: ContractValue::Void,
                    gas_used: gas_meter.gas_used(),
                    gas_remaining: gas_meter.gas_remaining(),
                    events: vec![], // Events are discarded on failure
                    error: Some(e.to_string()),
                })
            }
        }
    }

    /// Check if a contract is deployed at the given address.
    pub fn is_deployed(&self, address: &Address) -> bool {
        self.contracts.contains_key(address)
    }

    /// Get the bytecode hash of a deployed contract.
    pub fn contract_bytecode_hash(&self, address: &Address) -> Option<[u8; 32]> {
        self.contracts.get(address).map(|c| c.bytecode_hash)
    }

    /// Get the permissions of a deployed contract.
    pub fn contract_permissions(&self, address: &Address) -> Option<&ContractPermissions> {
        self.contracts.get(address).map(|c| &c.permissions)
    }

    /// Get the sandbox configuration.
    pub fn sandbox_config(&self) -> &SandboxConfig {
        &self.sandbox_config
    }

    /// Get a mutable reference to the sandbox configuration.
    /// WHY: Governance may adjust resource limits at runtime.
    pub fn sandbox_config_mut(&mut self) -> &mut SandboxConfig {
        &mut self.sandbox_config
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_functions::HostEnvironment;
    use crate::runtime::MockRuntime;
    use crate::sandbox::ContractPermissions;

    fn valid_wasm_bytecode() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    fn make_vm() -> GratiaVm {
        GratiaVm::new(Box::new(MockRuntime::new()))
    }

    fn make_host_env() -> HostEnvironment {
        HostEnvironment::new(100, 1700000000, Address([1u8; 32]), 5_000_000)
    }

    #[test]
    fn test_deploy_contract() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let addr = vm
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();

        assert!(vm.is_deployed(&addr));
        assert!(vm.contract_bytecode_hash(&addr).is_some());
    }

    #[test]
    fn test_deploy_invalid_bytecode() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bad_bytecode = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0x00];

        let result = vm.deploy_contract(&deployer, &bad_bytecode, ContractPermissions::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_deploy_bytecode_too_large() {
        let mut vm = GratiaVm::with_config(
            Box::new(MockRuntime::new()),
            SandboxConfig {
                max_bytecode_size: 4, // Tiny limit
                ..Default::default()
            },
        );
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let result = vm.deploy_contract(&deployer, &bytecode, ContractPermissions::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_call_contract_success() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let contract_addr = vm
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();

        let call = ContractCall {
            caller: Address([2u8; 32]),
            contract_address: contract_addr,
            function_name: "do_something".to_string(),
            args: vec![],
            gas_limit: 1_000_000,
        };

        let mut env = make_host_env();
        let result = vm.call_contract(&call, &mut env).unwrap();

        assert!(result.success);
        assert!(result.gas_used > 0);
        assert!(result.gas_remaining > 0);
    }

    #[test]
    fn test_call_contract_with_handler() {
        let mut mock = MockRuntime::new();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        // We need to deploy first to get the address, then register the handler.
        // Since the address is deterministic, compute it.
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-contract-v1:");
        hasher.update(&deployer.0);
        hasher.update(&bytecode);
        let result = hasher.finalize();
        let mut addr_bytes = [0u8; 32];
        addr_bytes.copy_from_slice(&result);
        let contract_addr = Address(addr_bytes);

        mock.register_handler(contract_addr, "multiply", |args| {
            if let (Some(ContractValue::I32(a)), Some(ContractValue::I32(b))) =
                (args.first(), args.get(1))
            {
                ContractValue::I32(a * b)
            } else {
                ContractValue::I32(0)
            }
        });

        let mut vm = GratiaVm::new(Box::new(mock));
        let deployed_addr = vm
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();
        assert_eq!(deployed_addr, contract_addr);

        let call = ContractCall {
            caller: Address([2u8; 32]),
            contract_address: contract_addr,
            function_name: "multiply".to_string(),
            args: vec![ContractValue::I32(6), ContractValue::I32(7)],
            gas_limit: 1_000_000,
        };

        let mut env = make_host_env();
        let result = vm.call_contract(&call, &mut env).unwrap();

        assert!(result.success);
        assert_eq!(result.return_value, ContractValue::I32(42));
    }

    #[test]
    fn test_call_contract_not_deployed() {
        let mut vm = make_vm();
        let call = ContractCall {
            caller: Address([1u8; 32]),
            contract_address: Address([99u8; 32]),
            function_name: "func".to_string(),
            args: vec![],
            gas_limit: 1_000_000,
        };

        let mut env = make_host_env();
        let result = vm.call_contract(&call, &mut env);
        assert!(result.is_err());
    }

    #[test]
    fn test_call_contract_out_of_gas() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let contract_addr = vm
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();

        let call = ContractCall {
            caller: Address([2u8; 32]),
            contract_address: contract_addr,
            function_name: "func".to_string(),
            args: vec![],
            gas_limit: 1, // Too little gas
        };

        let mut env = make_host_env();
        let result = vm.call_contract(&call, &mut env).unwrap();

        // Out of gas is a successful protocol operation (not an error),
        // but the contract execution itself failed.
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("out of gas"));
    }

    #[test]
    fn test_deterministic_address_derivation() {
        let mut vm1 = make_vm();
        let mut vm2 = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let addr1 = vm1
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();
        let addr2 = vm2
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();

        // Same deployer + same bytecode = same address.
        assert_eq!(addr1, addr2);
    }

    #[test]
    fn test_different_deployer_different_address() {
        let mut vm = make_vm();
        let bytecode = valid_wasm_bytecode();

        let addr1 = vm
            .deploy_contract(
                &Address([1u8; 32]),
                &bytecode,
                ContractPermissions::default(),
            )
            .unwrap();
        let addr2 = vm
            .deploy_contract(
                &Address([2u8; 32]),
                &bytecode,
                ContractPermissions::default(),
            )
            .unwrap();

        assert_ne!(addr1, addr2);
    }

    #[test]
    fn test_contract_permissions_stored() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();
        let perms = ContractPermissions {
            location: true,
            proximity: true,
            ..Default::default()
        };

        let addr = vm
            .deploy_contract(&deployer, &bytecode, perms)
            .unwrap();

        let stored_perms = vm.contract_permissions(&addr).unwrap();
        assert!(stored_perms.location);
        assert!(stored_perms.proximity);
        assert!(!stored_perms.presence);
    }

    #[test]
    fn test_sandbox_config_adjustable() {
        let mut vm = make_vm();
        assert_eq!(vm.sandbox_config().max_execution_time_ms, 500);

        // Simulate governance changing the limit.
        vm.sandbox_config_mut().max_execution_time_ms = 1000;
        assert_eq!(vm.sandbox_config().max_execution_time_ms, 1000);
    }

    #[test]
    fn test_gas_accounting_round_trip() {
        let mut vm = make_vm();
        let deployer = Address([1u8; 32]);
        let bytecode = valid_wasm_bytecode();

        let contract_addr = vm
            .deploy_contract(&deployer, &bytecode, ContractPermissions::default())
            .unwrap();

        let gas_limit: Lux = 1_000_000;
        let call = ContractCall {
            caller: Address([2u8; 32]),
            contract_address: contract_addr,
            function_name: "func".to_string(),
            args: vec![],
            gas_limit,
        };

        let mut env = make_host_env();
        let result = vm.call_contract(&call, &mut env).unwrap();

        // gas_used + gas_remaining should equal the original limit.
        assert_eq!(result.gas_used + result.gas_remaining, gas_limit);
    }
}
