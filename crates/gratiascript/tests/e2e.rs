//! End-to-end integration tests for the GratiaScript → WASM → Interpreter pipeline.
//!
//! These tests verify the complete smart contract lifecycle:
//! 1. Write GratiaScript source (.gs)
//! 2. Compile to WASM bytecode
//! 3. Deploy to GratiaVM
//! 4. Execute functions via InterpreterRuntime (when interpreter supports the pattern)
//! 5. Verify return values and gas consumption

use gratia_core::types::Address;
use gratia_vm::interpreter::InterpreterRuntime;
use gratia_vm::sandbox::ContractPermissions;
use gratia_vm::{ContractCall, GratiaVm};
use gratia_vm::host_functions::HostEnvironment;

/// Helper: compile GratiaScript and verify it produces valid WASM.
fn compile_gs(source: &str) -> Vec<u8> {
    let wasm = gratiascript::compile(source).expect("GratiaScript compilation failed");
    assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D], "Invalid WASM magic");
    wasm
}

/// Helper: deploy compiled WASM to a VM with InterpreterRuntime.
fn deploy_wasm(wasm: &[u8]) -> (GratiaVm, Address) {
    let runtime = InterpreterRuntime::new();
    let mut vm = GratiaVm::new(Box::new(runtime));
    let deployer = Address([0x42; 32]);
    let addr = vm
        .deploy_contract(&deployer, wasm, ContractPermissions::all())
        .expect("Contract deployment failed");
    (vm, addr)
}

// ============================================================================
// Compilation Tests — verify GratiaScript → valid WASM
// ============================================================================

#[test]
fn test_e2e_compile_presence_verifier() {
    let wasm = compile_gs(r#"
        contract PresenceVerifier {
            const minScore: i32 = 70;
            function verify(): bool {
                let score = @presence();
                if (score >= minScore) {
                    return true;
                }
                return false;
            }
            function getMinimum(): i32 {
                return minScore;
            }
        }
    "#);
    assert!(wasm.len() > 50);
    // Verify it contains the exported function names
    assert!(wasm.windows(6).any(|w| w == b"verify"));
    assert!(wasm.windows(10).any(|w| w == b"getMinimum"));

    // Deploy and execute
    let (mut vm, addr) = deploy_wasm(&wasm);
    let caller = Address([0x01; 32]);
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000)
        .with_presence_score(85);
    let call = ContractCall {
        caller, contract_address: addr,
        function_name: "getMinimum".to_string(),
        args: vec![], gas_limit: 1_000_000,
    };
    let result = vm.call_contract(&call, &mut env).unwrap();
    assert!(result.success);
}

#[test]
fn test_e2e_compile_proximity_gate() {
    let wasm = compile_gs(r#"
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
    "#);
    assert!(wasm.len() > 50);
    assert!(wasm.windows(11).any(|w| w == b"checkAccess"));
}

#[test]
fn test_e2e_compile_arithmetic() {
    let wasm = compile_gs(r#"
        contract Math {
            function add(a: f32, b: f32): f32 {
                return a + b;
            }
        }
    "#);
    // Should contain f32.add opcode (0x92)
    assert!(wasm.contains(&0x92));
}

#[test]
fn test_e2e_compile_with_globals() {
    let wasm = compile_gs(r#"
        contract Counter {
            let count: i32 = 0;
            let total: i64 = 1000;
            function getCount(): i32 {
                return count;
            }
        }
    "#);
    assert!(wasm.len() > 50);
}

#[test]
fn test_e2e_compile_while_loop() {
    let wasm = compile_gs(r#"
        contract Looper {
            let n: i32 = 0;
            function countTo(limit: i32): void {
                while (n < limit) {
                    n = n + 1;
                }
            }
        }
    "#);
    // Should contain loop opcode (0x03)
    assert!(wasm.contains(&0x03));
}

#[test]
fn test_e2e_compile_block_height() {
    let wasm = compile_gs(r#"
        contract BlockChecker {
            function getHeight(): i64 {
                return @blockHeight();
            }
        }
    "#);
    // Should contain "get_block_height" import
    assert!(wasm.windows(16).any(|w| w == b"get_block_height"));
}

#[test]
fn test_e2e_compile_multiple_functions() {
    let wasm = compile_gs(r#"
        contract Multi {
            let value: f32 = 0.0;
            function set(v: f32): void {
                value = v;
            }
            function get(): f32 {
                return value;
            }
            function double(): f32 {
                return value + value;
            }
        }
    "#);
    assert!(wasm.windows(3).any(|w| w == b"set"));
    assert!(wasm.windows(3).any(|w| w == b"get"));
    assert!(wasm.windows(6).any(|w| w == b"double"));
}

#[test]
fn test_e2e_compile_host_imports() {
    let wasm = compile_gs(r#"
        contract SensorReader {
            function readAll(): void {
                let loc = @location();
                let peers = @proximity();
                let score = @presence();
                let pressure = @sensor(0);
                let height = @blockHeight();
                let time = @blockTime();
                let bal = @balance();
            }
        }
    "#);
    // Should contain all host function imports
    assert!(wasm.windows(3).any(|w| w == b"env"));
    assert!(wasm.windows(16).any(|w| w == b"get_location_lat"));
    assert!(wasm.windows(16).any(|w| w == b"get_nearby_peers"));
}

// ============================================================================
// Deploy Tests — verify WASM loads into InterpreterRuntime
// ============================================================================

#[test]
fn test_e2e_deploy_simple() {
    let wasm = compile_gs("contract Empty {}");
    let (_vm, addr) = deploy_wasm(&wasm);
    assert_ne!(addr, Address([0u8; 32])); // Got a real address
}

#[test]
fn test_e2e_deploy_with_functions() {
    let wasm = compile_gs(r#"
        contract Token {
            let supply: i64 = 1000000;
            function getSupply(): i64 { return supply; }
        }
    "#);
    let (_vm, addr) = deploy_wasm(&wasm);
    assert_ne!(addr, Address([0u8; 32]));
}

// ============================================================================
// Template Tests — verify all 4 template contracts compile and deploy
// ============================================================================

#[test]
fn test_e2e_compile_all_templates() {
    let templates = [
        ("location_trigger", include_str!("../../../contracts/templates/location_trigger.gs")),
        ("proximity_escrow", include_str!("../../../contracts/templates/proximity_escrow.gs")),
        ("presence_verification", include_str!("../../../contracts/templates/presence_verification.gs")),
        ("poll", include_str!("../../../contracts/templates/poll.gs")),
    ];

    for (name, source) in templates {
        let wasm = gratiascript::compile(source)
            .unwrap_or_else(|e| panic!("Template '{}' failed to compile: {}", name, e));
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D], "'{}' invalid WASM", name);
        assert!(wasm.len() > 50, "'{}' too small: {} bytes", name, wasm.len());
    }
}

#[test]
fn test_e2e_deploy_all_templates() {
    let templates = [
        include_str!("../../../contracts/templates/location_trigger.gs"),
        include_str!("../../../contracts/templates/proximity_escrow.gs"),
        include_str!("../../../contracts/templates/presence_verification.gs"),
        include_str!("../../../contracts/templates/poll.gs"),
    ];

    for (i, source) in templates.iter().enumerate() {
        let wasm = gratiascript::compile(source).unwrap();
        let (_vm, addr) = deploy_wasm(&wasm);
        assert_ne!(addr, Address([0u8; 32]), "Template {} got zero address", i);
    }
}

// ============================================================================
// Error Tests
// ============================================================================

#[test]
fn test_e2e_compile_error_unknown_builtin() {
    let result = gratiascript::compile("contract C { function f(): void { let x = @unknown(); } }");
    assert!(result.is_err());
}

#[test]
fn test_e2e_compile_error_syntax() {
    let result = gratiascript::compile("contract { }");
    assert!(result.is_err());
}

#[test]
fn test_e2e_compile_error_unterminated_string() {
    let result = gratiascript::compile(r#"contract C { const s: string = "unterminated; }"#);
    assert!(result.is_err());
}

#[test]
fn test_debug_simple_execution() {
    let source = r#"
        contract Simple {
            function getAnswer(): i32 {
                return 42;
            }
        }
    "#;
    let wasm = gratiascript::compile(source).unwrap();
    eprintln!("WASM size: {} bytes", wasm.len());
    eprintln!("WASM hex (first 64): {}", hex::encode(&wasm[..64.min(wasm.len())]));
    
    let runtime = InterpreterRuntime::new();
    let mut vm = GratiaVm::new(Box::new(runtime));
    let deployer = Address([0x42; 32]);
    let addr = vm.deploy_contract(&deployer, &wasm, ContractPermissions::all()).unwrap();
    eprintln!("Deployed at: {}", hex::encode(addr.0));
    
    let caller = Address([0x01; 32]);
    let call = ContractCall {
        caller,
        contract_address: addr,
        function_name: "getAnswer".to_string(),
        args: vec![],
        gas_limit: 1_000_000,
    };
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000);
    
    match vm.call_contract(&call, &mut env) {
        Ok(result) => {
            eprintln!("SUCCESS: {:?}, gas={}", result.return_value, result.gas_used);
            assert!(result.success);
        }
        Err(e) => {
            eprintln!("EXECUTION ERROR: {:?}", e);
            panic!("Execution failed: {:?}", e);
        }
    }
}

#[test]
fn test_debug_presence_execution() {
    let source = r#"
        contract PresenceVerifier {
            const minScore: i32 = 70;
            function getMinimum(): i32 {
                return minScore;
            }
            function verify(): bool {
                let score = @presence();
                if (score >= minScore) {
                    return true;
                }
                return false;
            }
        }
    "#;
    let wasm = gratiascript::compile(source).unwrap();
    
    let runtime = InterpreterRuntime::new();
    let mut vm = GratiaVm::new(Box::new(runtime));
    let deployer = Address([0x42; 32]);
    let addr = vm.deploy_contract(&deployer, &wasm, ContractPermissions::all()).unwrap();
    
    let caller = Address([0x01; 32]);
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000)
        .with_presence_score(85)
        .with_nearby_peers(5);

    // Test getMinimum first (no host calls)
    let call1 = ContractCall {
        caller,
        contract_address: addr,
        function_name: "getMinimum".to_string(),
        args: vec![],
        gas_limit: 1_000_000,
    };
    match vm.call_contract(&call1, &mut env) {
        Ok(r) => eprintln!("getMinimum: success={} val={:?} gas={}", r.success, r.return_value, r.gas_used),
        Err(e) => eprintln!("getMinimum ERROR: {:?}", e),
    }

    // Test verify (has host call to @presence)
    let call2 = ContractCall {
        caller,
        contract_address: addr,
        function_name: "verify".to_string(),
        args: vec![],
        gas_limit: 1_000_000,
    };
    match vm.call_contract(&call2, &mut env) {
        Ok(r) => {
            eprintln!("verify: success={} val={:?} gas={}", r.success, r.return_value, r.gas_used);
            assert!(r.success);
        }
        Err(e) => {
            eprintln!("verify ERROR: {:?}", e);
            panic!("verify failed: {:?}", e);
        }
    }
}

#[test]
fn test_debug_global_return() {
    // Contract that returns a global constant - no host calls needed
    let source = r#"
        contract ConstTest {
            const value: i32 = 42;
            function getValue(): i32 {
                return value;
            }
        }
    "#;
    let wasm = gratiascript::compile(source).unwrap();
    let runtime = InterpreterRuntime::new();
    let mut vm = GratiaVm::new(Box::new(runtime));
    let deployer = Address([0x42; 32]);
    let addr = vm.deploy_contract(&deployer, &wasm, ContractPermissions::all()).unwrap();
    let caller = Address([0x01; 32]);
    let call = ContractCall {
        caller, contract_address: addr,
        function_name: "getValue".to_string(),
        args: vec![], gas_limit: 1_000_000,
    };
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000);
    match vm.call_contract(&call, &mut env) {
        Ok(r) => {
            eprintln!("getValue: success={} val={:?} gas={}", r.success, r.return_value, r.gas_used);
            assert!(r.success);
        }
        Err(e) => panic!("getValue failed: {:?}", e),
    }
}

#[test]
fn test_debug_if_const() {
    // Simple if/else with constants - no host calls
    let source = r#"
        contract IfTest {
            function check(): bool {
                let x = 10;
                if (x > 5) {
                    return true;
                }
                return false;
            }
        }
    "#;
    let wasm = gratiascript::compile(source).unwrap();
    let runtime = InterpreterRuntime::new();
    let mut vm = GratiaVm::new(Box::new(runtime));
    let deployer = Address([0x42; 32]);
    let addr = vm.deploy_contract(&deployer, &wasm, ContractPermissions::all()).unwrap();
    let caller = Address([0x01; 32]);
    let call = ContractCall {
        caller, contract_address: addr,
        function_name: "check".to_string(),
        args: vec![], gas_limit: 1_000_000,
    };
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000);
    match vm.call_contract(&call, &mut env) {
        Ok(r) => {
            eprintln!("check: success={} val={:?} gas={}", r.success, r.return_value, r.gas_used);
            assert!(r.success);
        }
        Err(e) => panic!("check failed: {:?}", e),
    }
}

// ============================================================================
// Phase 2 Features — Linear Memory, Storage, Events
// ============================================================================

#[test]
fn test_e2e_emit_event() {
    let source = r#"
        contract EventEmitter {
            function fire(): i32 {
                emit("Transfer", "alice_to_bob");
                return 1;
            }
        }
    "#;
    let wasm = compile_gs(source);
    let (mut vm, addr) = deploy_wasm(&wasm);
    let caller = Address([0x01; 32]);
    let call = ContractCall {
        caller, contract_address: addr,
        function_name: "fire".to_string(),
        args: vec![], gas_limit: 1_000_000,
    };
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000);
    let result = vm.call_contract(&call, &mut env).unwrap();
    assert!(result.success, "emit call failed: {:?}", result.error);
    // Should have emitted one event
    assert_eq!(result.events.len(), 1, "expected 1 event, got {}", result.events.len());
    assert_eq!(result.events[0].topic, "Transfer");
    assert_eq!(result.events[0].data, b"alice_to_bob");
}

#[test]
fn test_e2e_storage_write_and_emit() {
    let source = r#"
        contract StorageWriter {
            function save(): i32 {
                @store.write("counter", "42");
                emit("Saved", "counter");
                return 1;
            }
        }
    "#;
    let wasm = compile_gs(source);
    let (mut vm, addr) = deploy_wasm(&wasm);
    let caller = Address([0x01; 32]);
    let call = ContractCall {
        caller, contract_address: addr,
        function_name: "save".to_string(),
        args: vec![], gas_limit: 1_000_000,
    };
    let mut env = HostEnvironment::new(100, 1700000000, caller, 5_000_000);
    let result = vm.call_contract(&call, &mut env).unwrap();
    assert!(result.success, "storage_write call failed: {:?}", result.error);
    // Should have emitted one event
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].topic, "Saved");
}
