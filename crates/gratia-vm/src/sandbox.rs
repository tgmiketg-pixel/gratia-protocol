//! Contract sandboxing and resource limits for GratiaVM.
//!
//! Every contract execution is sandboxed with strict resource limits
//! designed for mobile devices. These limits ensure that no single
//! contract can monopolize CPU, memory, or storage on a phone.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// Sandbox Errors
// ============================================================================

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    #[error("memory limit exceeded: {used_bytes} bytes used, {limit_bytes} bytes allowed")]
    MemoryLimitExceeded { used_bytes: usize, limit_bytes: usize },

    #[error("execution time limit exceeded: {elapsed_ms}ms used, {limit_ms}ms allowed")]
    TimeLimitExceeded { elapsed_ms: u64, limit_ms: u64 },

    #[error("stack depth limit exceeded: {depth} frames, {limit} allowed")]
    StackDepthExceeded { depth: u32, limit: u32 },

    #[error("call depth limit exceeded: {depth} calls, {limit} allowed")]
    CallDepthExceeded { depth: u32, limit: u32 },

    #[error("bytecode too large: {size} bytes, {limit} bytes allowed")]
    BytecodeTooLarge { size: usize, limit: usize },

    #[error("bytecode validation failed: {reason}")]
    InvalidBytecode { reason: String },

    #[error("permission denied: contract lacks '{permission}' permission")]
    PermissionDenied { permission: String },
}

// ============================================================================
// Sandbox Configuration
// ============================================================================

/// Resource limits for contract execution.
///
/// These defaults are tuned for mid-range smartphones (2+ GB RAM, quad-core ARM).
/// They can be adjusted via governance for the entire network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Maximum WASM linear memory a contract can use (bytes).
    /// WHY: 256 MB is generous for a smart contract but prevents a single
    /// contract from exhausting the phone's RAM. Most phones have 2-4 GB
    /// total; we reserve 256 MB as the absolute ceiling for one execution.
    pub max_memory_bytes: usize,

    /// Maximum wall-clock execution time (milliseconds).
    /// WHY: 500ms keeps contract execution within a single block time (3-5s).
    /// Validators need time for consensus, networking, and other transactions
    /// within the same block, so no single contract can consume more than
    /// ~10-15% of the block production window.
    pub max_execution_time_ms: u64,

    /// Maximum WASM stack depth (frames).
    /// WHY: Deep recursion on mobile can blow the native stack. 1024 frames
    /// is sufficient for any reasonable contract logic while preventing
    /// stack overflow attacks.
    pub max_stack_depth: u32,

    /// Maximum cross-contract call depth.
    /// WHY: Each cross-contract call saves execution state. At 8 levels deep,
    /// memory usage for saved contexts stays bounded. This also limits
    /// reentrancy attack surface.
    pub max_call_depth: u32,

    /// Maximum bytecode size (bytes) for a single contract.
    /// WHY: 1 MB covers complex contracts (for reference, Uniswap V3 is ~25 KB
    /// compiled). Larger bytecode wastes block space and takes longer to
    /// compile on mobile. State storage target is 2-5 GB total, so individual
    /// contracts must be small.
    pub max_bytecode_size: usize,

    /// Maximum number of WASM memory pages a contract can allocate.
    /// WHY: Each WASM page is 64 KB. 4096 pages = 256 MB, matching
    /// max_memory_bytes. This is the WASM-native limit enforcement.
    pub max_memory_pages: u32,

    /// Maximum number of events a contract can emit per execution.
    /// WHY: Events are included in blocks and propagated to all validators.
    /// Unlimited events would let a contract spam the network. 256 events
    /// per execution is generous for any legitimate use case.
    pub max_events_per_execution: usize,

    /// Maximum total event data size per execution (bytes).
    /// WHY: Even with limited event count, each event could carry large data.
    /// 64 KB total bounds the block-space impact of events.
    pub max_event_data_bytes: usize,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            max_memory_bytes: 256 * 1024 * 1024,     // 256 MB
            max_execution_time_ms: 500,                // 500ms
            max_stack_depth: 1024,                     // 1024 frames
            max_call_depth: 8,                         // 8 cross-contract calls deep
            max_bytecode_size: 1024 * 1024,            // 1 MB
            max_memory_pages: 4096,                    // 4096 * 64KB = 256 MB
            max_events_per_execution: 256,             // 256 events
            max_event_data_bytes: 64 * 1024,           // 64 KB total event data
        }
    }
}

// ============================================================================
// Contract Permissions
// ============================================================================

/// Permissions granted to a contract for accessing host functions.
///
/// Contracts declare required permissions at deployment time. Users can
/// review permissions before interacting with a contract (similar to
/// Android app permissions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractPermissions {
    /// Can call @location (get_location).
    /// WHY: Location data is privacy-sensitive. Contracts must explicitly
    /// request this, and users see it before calling the contract.
    pub location: bool,

    /// Can call @proximity (get_nearby_peers).
    /// WHY: Peer count can reveal information about the user's environment.
    pub proximity: bool,

    /// Can call @presence (get_presence_score).
    /// WHY: Presence score is semi-public (used in VRF), but contracts
    /// should declare if they use it so users know.
    pub presence: bool,

    /// Can call @sensor (get_sensor_data).
    /// WHY: Sensor readings (barometer, light, etc.) can fingerprint
    /// a user's environment. Explicit permission required.
    pub sensor: bool,

    /// Can perform cross-contract calls.
    /// WHY: Cross-contract calls introduce reentrancy risk and resource
    /// amplification. Contracts that don't need it shouldn't have it.
    pub cross_contract_calls: bool,
}

impl ContractPermissions {
    /// Create permissions with all capabilities enabled.
    pub fn all() -> Self {
        ContractPermissions {
            location: true,
            proximity: true,
            presence: true,
            sensor: true,
            cross_contract_calls: true,
        }
    }

    /// Check if a specific host function is permitted.
    pub fn check_permission(&self, function_name: &str) -> Result<(), SandboxError> {
        let permitted = match function_name {
            "get_location" => self.location,
            "get_nearby_peers" => self.proximity,
            "get_presence_score" => self.presence,
            "get_sensor_data" => self.sensor,
            // Block info and caller info are always available — they don't
            // leak privacy-sensitive data beyond what's on-chain already.
            "get_block_height" | "get_block_timestamp" => true,
            "get_caller_address" | "get_caller_balance" => true,
            _ => true,
        };

        if permitted {
            Ok(())
        } else {
            Err(SandboxError::PermissionDenied {
                permission: function_name.to_string(),
            })
        }
    }
}

// ============================================================================
// Sandboxed Execution
// ============================================================================

/// Tracks resource usage during a sandboxed contract execution.
///
/// Created at the start of each contract call and checked throughout
/// execution. If any limit is exceeded, execution is aborted.
pub struct SandboxedExecution {
    config: SandboxConfig,
    permissions: ContractPermissions,
    start_time: Instant,
    current_memory_bytes: usize,
    current_stack_depth: u32,
    current_call_depth: u32,
    events_emitted: usize,
    event_data_bytes: usize,
}

impl SandboxedExecution {
    /// Begin a new sandboxed execution with the given config and permissions.
    pub fn new(config: SandboxConfig, permissions: ContractPermissions) -> Self {
        SandboxedExecution {
            config,
            permissions,
            start_time: Instant::now(),
            current_memory_bytes: 0,
            current_stack_depth: 0,
            current_call_depth: 0,
            events_emitted: 0,
            event_data_bytes: 0,
        }
    }

    /// Check if the execution time limit has been exceeded.
    pub fn enforce_time_limit(&self) -> Result<(), SandboxError> {
        let elapsed = self.start_time.elapsed();
        let limit = Duration::from_millis(self.config.max_execution_time_ms);
        if elapsed > limit {
            return Err(SandboxError::TimeLimitExceeded {
                elapsed_ms: elapsed.as_millis() as u64,
                limit_ms: self.config.max_execution_time_ms,
            });
        }
        Ok(())
    }

    /// Check and update memory usage.
    pub fn enforce_memory_limit(&mut self, memory_bytes: usize) -> Result<(), SandboxError> {
        if memory_bytes > self.config.max_memory_bytes {
            return Err(SandboxError::MemoryLimitExceeded {
                used_bytes: memory_bytes,
                limit_bytes: self.config.max_memory_bytes,
            });
        }
        self.current_memory_bytes = memory_bytes;
        Ok(())
    }

    /// Push a stack frame. Returns error if depth limit exceeded.
    pub fn push_stack_frame(&mut self) -> Result<(), SandboxError> {
        if self.current_stack_depth >= self.config.max_stack_depth {
            return Err(SandboxError::StackDepthExceeded {
                depth: self.current_stack_depth + 1,
                limit: self.config.max_stack_depth,
            });
        }
        self.current_stack_depth += 1;
        Ok(())
    }

    /// Pop a stack frame.
    pub fn pop_stack_frame(&mut self) {
        self.current_stack_depth = self.current_stack_depth.saturating_sub(1);
    }

    /// Enter a cross-contract call. Returns error if call depth limit exceeded.
    pub fn enter_call(&mut self) -> Result<(), SandboxError> {
        if self.current_call_depth >= self.config.max_call_depth {
            return Err(SandboxError::CallDepthExceeded {
                depth: self.current_call_depth + 1,
                limit: self.config.max_call_depth,
            });
        }
        self.current_call_depth += 1;
        Ok(())
    }

    /// Exit a cross-contract call.
    pub fn exit_call(&mut self) {
        self.current_call_depth = self.current_call_depth.saturating_sub(1);
    }

    /// Record an event emission. Returns error if event limits exceeded.
    pub fn record_event(&mut self, data_len: usize) -> Result<(), SandboxError> {
        self.events_emitted += 1;
        if self.events_emitted > self.config.max_events_per_execution {
            return Err(SandboxError::PermissionDenied {
                permission: format!(
                    "event limit exceeded: {} events, {} allowed",
                    self.events_emitted, self.config.max_events_per_execution
                ),
            });
        }

        self.event_data_bytes = self.event_data_bytes.checked_add(data_len)
            .ok_or_else(|| SandboxError::PermissionDenied {
                permission: "event data size overflow".to_string(),
            })?;
        if self.event_data_bytes > self.config.max_event_data_bytes {
            return Err(SandboxError::PermissionDenied {
                permission: format!(
                    "event data limit exceeded: {} bytes, {} allowed",
                    self.event_data_bytes, self.config.max_event_data_bytes
                ),
            });
        }

        Ok(())
    }

    /// Check if a host function call is permitted.
    pub fn check_permission(&self, function_name: &str) -> Result<(), SandboxError> {
        self.permissions.check_permission(function_name)
    }

    /// Get the elapsed execution time.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get current memory usage.
    pub fn memory_usage(&self) -> usize {
        self.current_memory_bytes
    }

    /// Get the sandbox configuration.
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get the contract permissions.
    pub fn permissions(&self) -> &ContractPermissions {
        &self.permissions
    }
}

// ============================================================================
// Bytecode Validation
// ============================================================================

/// Validate WASM bytecode before compilation.
///
/// Performs basic structural checks to reject obviously invalid or
/// malicious bytecode before spending resources on full compilation.
pub fn validate_bytecode(bytecode: &[u8], config: &SandboxConfig) -> Result<(), SandboxError> {
    // Check size limit.
    if bytecode.len() > config.max_bytecode_size {
        return Err(SandboxError::BytecodeTooLarge {
            size: bytecode.len(),
            limit: config.max_bytecode_size,
        });
    }

    // Check WASM magic number.
    // WHY: The WASM binary format always starts with \0asm (0x00 0x61 0x73 0x6d).
    // Rejecting non-WASM early avoids wasting time on the compiler.
    if bytecode.len() < 8 {
        return Err(SandboxError::InvalidBytecode {
            reason: "bytecode too short to be valid WASM (minimum 8 bytes for header)".to_string(),
        });
    }

    let wasm_magic = &[0x00, 0x61, 0x73, 0x6d]; // \0asm
    if &bytecode[0..4] != wasm_magic {
        return Err(SandboxError::InvalidBytecode {
            reason: "missing WASM magic number (\\0asm)".to_string(),
        });
    }

    // Check WASM version (must be 1).
    // WHY: WASM version 1 is the only stable version. Rejecting others
    // ensures we don't accidentally accept WASM 2.0 features that our
    // runtime may not support deterministically.
    let version = u32::from_le_bytes([bytecode[4], bytecode[5], bytecode[6], bytecode[7]]);
    if version != 1 {
        return Err(SandboxError::InvalidBytecode {
            reason: format!("unsupported WASM version: {} (only version 1 is supported)", version),
        });
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_wasm_header() -> Vec<u8> {
        // Minimal valid WASM header: magic + version 1
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    #[test]
    fn test_validate_bytecode_valid() {
        let config = SandboxConfig::default();
        let bytecode = valid_wasm_header();
        assert!(validate_bytecode(&bytecode, &config).is_ok());
    }

    #[test]
    fn test_validate_bytecode_too_large() {
        let mut config = SandboxConfig::default();
        config.max_bytecode_size = 4; // Tiny limit for testing
        let bytecode = valid_wasm_header();
        let result = validate_bytecode(&bytecode, &config);
        assert!(matches!(result, Err(SandboxError::BytecodeTooLarge { .. })));
    }

    #[test]
    fn test_validate_bytecode_too_short() {
        let config = SandboxConfig::default();
        let bytecode = vec![0x00, 0x61];
        let result = validate_bytecode(&bytecode, &config);
        assert!(matches!(result, Err(SandboxError::InvalidBytecode { .. })));
    }

    #[test]
    fn test_validate_bytecode_bad_magic() {
        let config = SandboxConfig::default();
        let bytecode = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0x00];
        let result = validate_bytecode(&bytecode, &config);
        assert!(matches!(result, Err(SandboxError::InvalidBytecode { .. })));
    }

    #[test]
    fn test_validate_bytecode_wrong_version() {
        let config = SandboxConfig::default();
        let bytecode = vec![0x00, 0x61, 0x73, 0x6d, 0x02, 0x00, 0x00, 0x00]; // Version 2
        let result = validate_bytecode(&bytecode, &config);
        assert!(matches!(result, Err(SandboxError::InvalidBytecode { .. })));
    }

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert_eq!(config.max_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(config.max_execution_time_ms, 500);
        assert_eq!(config.max_stack_depth, 1024);
        assert_eq!(config.max_call_depth, 8);
        assert_eq!(config.max_bytecode_size, 1024 * 1024);
    }

    #[test]
    fn test_permissions_default_denies_all() {
        let perms = ContractPermissions::default();
        assert!(perms.check_permission("get_location").is_err());
        assert!(perms.check_permission("get_nearby_peers").is_err());
        assert!(perms.check_permission("get_sensor_data").is_err());

        // Block info is always allowed.
        assert!(perms.check_permission("get_block_height").is_ok());
        assert!(perms.check_permission("get_caller_address").is_ok());
    }

    #[test]
    fn test_permissions_all() {
        let perms = ContractPermissions::all();
        assert!(perms.check_permission("get_location").is_ok());
        assert!(perms.check_permission("get_nearby_peers").is_ok());
        assert!(perms.check_permission("get_presence_score").is_ok());
        assert!(perms.check_permission("get_sensor_data").is_ok());
    }

    #[test]
    fn test_sandboxed_execution_memory_limit() {
        let config = SandboxConfig {
            max_memory_bytes: 1024,
            ..Default::default()
        };
        let mut exec = SandboxedExecution::new(config, ContractPermissions::default());

        assert!(exec.enforce_memory_limit(512).is_ok());
        assert_eq!(exec.memory_usage(), 512);

        assert!(exec.enforce_memory_limit(2048).is_err());
    }

    #[test]
    fn test_sandboxed_execution_stack_depth() {
        let config = SandboxConfig {
            max_stack_depth: 3,
            ..Default::default()
        };
        let mut exec = SandboxedExecution::new(config, ContractPermissions::default());

        assert!(exec.push_stack_frame().is_ok()); // 1
        assert!(exec.push_stack_frame().is_ok()); // 2
        assert!(exec.push_stack_frame().is_ok()); // 3
        assert!(exec.push_stack_frame().is_err()); // 4 > 3

        exec.pop_stack_frame(); // back to 2
        assert!(exec.push_stack_frame().is_ok()); // 3 again
    }

    #[test]
    fn test_sandboxed_execution_call_depth() {
        let config = SandboxConfig {
            max_call_depth: 2,
            ..Default::default()
        };
        let mut exec = SandboxedExecution::new(config, ContractPermissions::default());

        assert!(exec.enter_call().is_ok()); // 1
        assert!(exec.enter_call().is_ok()); // 2
        assert!(exec.enter_call().is_err()); // 3 > 2

        exec.exit_call(); // back to 1
        assert!(exec.enter_call().is_ok()); // 2 again
    }

    #[test]
    fn test_sandboxed_execution_event_limits() {
        let config = SandboxConfig {
            max_events_per_execution: 2,
            max_event_data_bytes: 100,
            ..Default::default()
        };
        let mut exec = SandboxedExecution::new(config, ContractPermissions::default());

        assert!(exec.record_event(10).is_ok()); // 1 event, 10 bytes
        assert!(exec.record_event(10).is_ok()); // 2 events, 20 bytes
        assert!(exec.record_event(10).is_err()); // 3 events > 2 limit
    }

    #[test]
    fn test_sandboxed_execution_event_data_limit() {
        let config = SandboxConfig {
            max_events_per_execution: 100,
            max_event_data_bytes: 50,
            ..Default::default()
        };
        let mut exec = SandboxedExecution::new(config, ContractPermissions::default());

        assert!(exec.record_event(30).is_ok()); // 30 bytes
        assert!(exec.record_event(30).is_err()); // 60 bytes > 50 limit
    }

    #[test]
    fn test_sandboxed_execution_permission_check() {
        let perms = ContractPermissions {
            location: true,
            proximity: false,
            ..Default::default()
        };
        let exec = SandboxedExecution::new(SandboxConfig::default(), perms);

        assert!(exec.check_permission("get_location").is_ok());
        assert!(exec.check_permission("get_nearby_peers").is_err());
        assert!(exec.check_permission("get_block_height").is_ok());
    }
}
