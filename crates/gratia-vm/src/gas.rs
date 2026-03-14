//! ARM compute cycle gas metering for GratiaVM.
//!
//! Gas costs are calibrated to approximate ARM compute cycles on a
//! mid-range smartphone (e.g., Snapdragon 600-series). The goal is
//! deterministic, predictable cost accounting so that validators can
//! enforce identical resource limits across heterogeneous devices.

use std::fmt;

use thiserror::Error;

// ============================================================================
// Gas Error
// ============================================================================

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum GasError {
    #[error("out of gas: used {used}, limit {limit}")]
    OutOfGas { used: u64, limit: u64 },
}

// ============================================================================
// Gas Cost Table
// ============================================================================

/// Gas costs calibrated to approximate ARM compute cycles.
///
/// These values are tuned so that the gas limit maps roughly to a real
/// time budget on ARM hardware. A 500ms execution window at ~1 GHz
/// effective throughput gives ~500M cycles. Gas units approximate
/// cycles-per-operation at the ARM ISA level.
#[derive(Debug, Clone)]
pub struct GasCosts {
    // -- Basic operations --

    /// Cost of a single memory load/store.
    /// WHY: ARM LDR/STR is 1 cycle on most Cortex-A cores with L1 cache hit.
    pub memory_access: u64,

    /// Cost of basic arithmetic (add, sub, mul, bitwise ops).
    /// WHY: Single-cycle on ARM Cortex-A in-order and out-of-order pipelines.
    pub arithmetic: u64,

    /// Cost of integer division.
    /// WHY: ARM SDIV/UDIV takes 2-12 cycles depending on operand size;
    /// we use 5 as a representative average for 32-bit operands.
    pub division: u64,

    /// Cost of a comparison and branch.
    /// WHY: CMP + branch is 1 cycle when predicted correctly on ARM;
    /// using 2 to account for occasional mispredictions averaging out.
    pub branch: u64,

    // -- Crypto operations --

    /// Cost of SHA-256 hash (32 bytes input).
    /// WHY: ARM Cryptography Extensions (CE) compute SHA-256 in ~10 cycles/byte
    /// for small inputs. 32 bytes * 10 + setup overhead ~= 1000 gas.
    pub sha256_hash: u64,

    /// Cost of Ed25519 signature verification.
    /// WHY: Ed25519 verify takes ~50-70 microseconds on ARM (~50,000 cycles
    /// at 1 GHz). This is the most expensive standard operation contracts
    /// might perform, so it must be priced to prevent DoS via verify spam.
    pub ed25519_verify: u64,

    // -- Host function calls (mobile-native opcodes) --

    /// Cost of calling any host function (base overhead).
    /// WHY: Crossing the WASM-to-host boundary involves context switch,
    /// argument marshaling, and validation. ~100 cycles on ARM.
    pub host_call_base: u64,

    /// Cost of @location (get_location).
    /// WHY: Reading cached GPS coordinates from the host environment is
    /// cheap — just memory access + serialization. Slightly more than
    /// base because it touches sensor state.
    pub host_get_location: u64,

    /// Cost of @proximity (get_nearby_peers).
    /// WHY: Querying the Bluetooth/Wi-Fi peer cache involves iterating
    /// a small list. More expensive than a simple read.
    pub host_get_proximity: u64,

    /// Cost of @presence (get_presence_score).
    /// WHY: Single cached u8 read. Minimal overhead beyond the base call.
    pub host_get_presence: u64,

    /// Cost of @sensor (get_sensor_data).
    /// WHY: Sensor reads are cached on-device; the host function just
    /// returns the cached value. Comparable to get_location.
    pub host_get_sensor: u64,

    /// Cost of get_block_height / get_block_timestamp.
    /// WHY: These are simple scalar reads from the execution context.
    pub host_get_block_info: u64,

    /// Cost of get_caller_address / get_caller_balance.
    /// WHY: Address is in the execution context; balance requires a
    /// state lookup (cached), hence slightly more expensive.
    pub host_get_caller_info: u64,

    // -- Storage operations --

    /// Cost of reading from contract storage (per 32-byte slot).
    /// WHY: RocksDB point read on mobile flash is ~50-200 microseconds.
    /// We price at 200 gas per slot to discourage excessive storage reads
    /// that would thrash the flash storage on low-end phones.
    pub storage_read: u64,

    /// Cost of writing to contract storage (per 32-byte slot).
    /// WHY: Flash writes are 10-50x more expensive than reads due to
    /// write amplification and wear leveling. At 5000 gas per slot,
    /// contracts are strongly incentivized to minimize state writes —
    /// critical for mobile NAND longevity.
    pub storage_write: u64,

    // -- Memory operations --

    /// Cost of allocating a page of WASM linear memory (64 KB).
    /// WHY: WASM memory.grow triggers a system mmap/VirtualAlloc and
    /// zero-fill. On mobile, memory is scarce, so we price growth to
    /// discourage contracts from requesting excessive memory.
    pub memory_grow_page: u64,

    // -- Contract-to-contract calls --

    /// Cost of calling another contract (base overhead).
    /// WHY: Cross-contract calls require saving/restoring execution state,
    /// loading the callee module, and setting up a new sandbox. This is
    /// expensive and must be priced to prevent call-depth bombing.
    pub cross_contract_call: u64,

    // -- Logging --

    /// Cost per byte of emitting a log/event.
    /// WHY: Events are included in blocks and propagated to all validators.
    /// Per-byte pricing prevents contracts from spamming large event data.
    pub log_per_byte: u64,
}

impl Default for GasCosts {
    fn default() -> Self {
        GasCosts {
            memory_access: 1,
            arithmetic: 1,
            division: 5,
            branch: 2,
            sha256_hash: 1_000,
            ed25519_verify: 50_000,
            host_call_base: 100,
            host_get_location: 150,
            host_get_proximity: 300,
            host_get_presence: 120,
            host_get_sensor: 150,
            host_get_block_info: 110,
            host_get_caller_info: 200,
            storage_read: 200,
            storage_write: 5_000,
            memory_grow_page: 10_000,
            cross_contract_call: 100_000,
            log_per_byte: 2,
        }
    }
}

// ============================================================================
// GasMeter
// ============================================================================

/// Tracks gas consumption during contract execution.
///
/// The meter is initialized with a gas limit (set by the transaction sender)
/// and is charged incrementally as the contract executes. If the limit is
/// exceeded, execution is aborted and all state changes are reverted.
#[derive(Debug, Clone)]
pub struct GasMeter {
    /// Maximum gas allowed for this execution.
    limit: u64,
    /// Gas consumed so far.
    used: u64,
    /// Gas cost table.
    costs: GasCosts,
}

impl GasMeter {
    /// Create a new GasMeter with the given limit and default costs.
    pub fn new(limit: u64) -> Self {
        GasMeter {
            limit,
            used: 0,
            costs: GasCosts::default(),
        }
    }

    /// Create a new GasMeter with custom gas costs.
    pub fn with_costs(limit: u64, costs: GasCosts) -> Self {
        GasMeter {
            limit,
            used: 0,
            costs,
        }
    }

    /// Charge gas for an operation. Returns an error if the limit is exceeded.
    pub fn charge(&mut self, amount: u64) -> Result<(), GasError> {
        let new_used = self.used.saturating_add(amount);
        if new_used > self.limit {
            // WHY: We set used to limit (not new_used) so gas_used() never
            // exceeds gas_limit(), keeping billing deterministic.
            self.used = self.limit;
            return Err(GasError::OutOfGas {
                used: new_used,
                limit: self.limit,
            });
        }
        self.used = new_used;
        Ok(())
    }

    /// Charge for a memory access.
    pub fn charge_memory_access(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.memory_access)
    }

    /// Charge for a basic arithmetic operation.
    pub fn charge_arithmetic(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.arithmetic)
    }

    /// Charge for a division operation.
    pub fn charge_division(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.division)
    }

    /// Charge for a branch/comparison.
    pub fn charge_branch(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.branch)
    }

    /// Charge for a SHA-256 hash.
    pub fn charge_sha256(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.sha256_hash)
    }

    /// Charge for an Ed25519 signature verification.
    pub fn charge_ed25519_verify(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.ed25519_verify)
    }

    /// Charge for a host function call by name.
    pub fn charge_host_call(&mut self, function_name: &str) -> Result<(), GasError> {
        let cost = match function_name {
            "get_location" => self.costs.host_get_location,
            "get_nearby_peers" => self.costs.host_get_proximity,
            "get_presence_score" => self.costs.host_get_presence,
            "get_sensor_data" => self.costs.host_get_sensor,
            "get_block_height" | "get_block_timestamp" => self.costs.host_get_block_info,
            "get_caller_address" | "get_caller_balance" => self.costs.host_get_caller_info,
            _ => self.costs.host_call_base,
        };
        self.charge(cost)
    }

    /// Charge for a storage read.
    pub fn charge_storage_read(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.storage_read)
    }

    /// Charge for a storage write.
    pub fn charge_storage_write(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.storage_write)
    }

    /// Charge for growing WASM memory by the given number of pages.
    pub fn charge_memory_grow(&mut self, pages: u32) -> Result<(), GasError> {
        self.charge(self.costs.memory_grow_page * pages as u64)
    }

    /// Charge for a cross-contract call.
    pub fn charge_cross_contract_call(&mut self) -> Result<(), GasError> {
        self.charge(self.costs.cross_contract_call)
    }

    /// Charge for emitting log data.
    pub fn charge_log(&mut self, data_len: usize) -> Result<(), GasError> {
        self.charge(self.costs.log_per_byte * data_len as u64)
    }

    /// Gas remaining before the limit is hit.
    pub fn gas_remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// Gas consumed so far.
    pub fn gas_used(&self) -> u64 {
        self.used
    }

    /// The gas limit for this execution.
    pub fn gas_limit(&self) -> u64 {
        self.limit
    }

    /// Reference to the gas cost table.
    pub fn costs(&self) -> &GasCosts {
        &self.costs
    }
}

impl fmt::Display for GasMeter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GasMeter({}/{} used, {} remaining)",
            self.used,
            self.limit,
            self.gas_remaining()
        )
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gas_meter_basic() {
        let mut meter = GasMeter::new(1000);
        assert_eq!(meter.gas_remaining(), 1000);
        assert_eq!(meter.gas_used(), 0);

        meter.charge(100).unwrap();
        assert_eq!(meter.gas_remaining(), 900);
        assert_eq!(meter.gas_used(), 100);
    }

    #[test]
    fn test_gas_meter_out_of_gas() {
        let mut meter = GasMeter::new(100);
        meter.charge(50).unwrap();

        let result = meter.charge(60);
        assert!(result.is_err());
        match result.unwrap_err() {
            GasError::OutOfGas { used: 110, limit: 100 } => {}
            e => panic!("expected OutOfGas, got {:?}", e),
        }

        // WHY: After out-of-gas, used is capped at limit for deterministic billing.
        assert_eq!(meter.gas_used(), 100);
    }

    #[test]
    fn test_gas_meter_exact_limit() {
        let mut meter = GasMeter::new(100);
        meter.charge(100).unwrap();
        assert_eq!(meter.gas_remaining(), 0);

        // One more unit should fail.
        assert!(meter.charge(1).is_err());
    }

    #[test]
    fn test_charge_host_calls() {
        let mut meter = GasMeter::new(1_000_000);

        meter.charge_host_call("get_location").unwrap();
        assert_eq!(meter.gas_used(), 150);

        meter.charge_host_call("get_nearby_peers").unwrap();
        assert_eq!(meter.gas_used(), 150 + 300);

        meter.charge_host_call("get_presence_score").unwrap();
        assert_eq!(meter.gas_used(), 150 + 300 + 120);
    }

    #[test]
    fn test_charge_storage_ops() {
        let mut meter = GasMeter::new(1_000_000);

        meter.charge_storage_read().unwrap();
        assert_eq!(meter.gas_used(), 200);

        meter.charge_storage_write().unwrap();
        assert_eq!(meter.gas_used(), 200 + 5_000);
    }

    #[test]
    fn test_charge_crypto_ops() {
        let mut meter = GasMeter::new(1_000_000);

        meter.charge_sha256().unwrap();
        assert_eq!(meter.gas_used(), 1_000);

        meter.charge_ed25519_verify().unwrap();
        assert_eq!(meter.gas_used(), 1_000 + 50_000);
    }

    #[test]
    fn test_charge_log() {
        let mut meter = GasMeter::new(1_000_000);
        meter.charge_log(256).unwrap();
        // 256 bytes * 2 gas/byte = 512
        assert_eq!(meter.gas_used(), 512);
    }

    #[test]
    fn test_custom_costs() {
        let mut costs = GasCosts::default();
        costs.arithmetic = 10;

        let mut meter = GasMeter::with_costs(1000, costs);
        meter.charge_arithmetic().unwrap();
        assert_eq!(meter.gas_used(), 10);
    }

    #[test]
    fn test_display() {
        let meter = GasMeter::new(1000);
        let display = format!("{}", meter);
        assert!(display.contains("0/1000"));
    }
}
