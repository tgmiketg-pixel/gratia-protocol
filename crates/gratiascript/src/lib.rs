//! GratiaScript — TypeScript-derived smart contract language for the Gratia blockchain.
//!
//! GratiaScript compiles to WebAssembly bytecode that runs on GratiaVM. It provides
//! mobile-native opcodes (@location, @proximity, @presence, @sensor) that no other
//! smart contract language has — because no other blockchain runs on phones.
//!
//! ## Architecture
//!
//! ```text
//! Source (.gs) → Lexer → Tokens → Parser → AST → CodeGen → WASM bytecode
//! ```
//!
//! ## Example
//!
//! ```text
//! contract ProximityGate {
//!     const minPeers: i32 = 3;
//!
//!     function checkAccess(): bool {
//!         let peers = @proximity();
//!         if (peers >= minPeers) {
//!             emit("access_granted", "Enough peers nearby");
//!             return true;
//!         }
//!         return false;
//!     }
//! }
//! ```

pub mod ast;
pub mod codegen;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod token;
pub mod typechecker;

use error::CompileError;

/// Compile GratiaScript source code to WASM bytecode.
///
/// This is the main entry point for the compiler. It takes a GratiaScript
/// source string and returns the compiled WASM binary, ready to deploy
/// to GratiaVM via `GratiaVm::deploy_contract()`.
///
/// ## Errors
///
/// Returns `CompileError` for:
/// - Lexer errors (invalid characters, unterminated strings)
/// - Parse errors (syntax errors, unexpected tokens)
/// - Code generation errors (undefined variables, type mismatches)
pub fn compile(source: &str) -> Result<Vec<u8>, CompileError> {
    let tokens = lexer::Lexer::new(source).tokenize()?;
    let contract = parser::Parser::new(tokens).parse_contract()?;
    // WHY: Type check before code generation. Catches type errors (adding
    // i32 + f32, wrong return type, undefined variables) at compile time
    // instead of crashing the interpreter at runtime.
    let _typed = typechecker::TypeChecker::new().check(&contract)?;
    let wasm = codegen::CodeGen::new().compile(&contract)?;
    Ok(wasm)
}

/// Compile and return both the WASM bytecode and the contract name.
pub fn compile_named(source: &str) -> Result<(String, Vec<u8>), CompileError> {
    let tokens = lexer::Lexer::new(source).tokenize()?;
    let contract = parser::Parser::new(tokens).parse_contract()?;
    let name = contract.name.clone();
    let wasm = codegen::CodeGen::new().compile(&contract)?;
    Ok((name, wasm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_empty_contract() {
        let wasm = compile("contract Empty {}").unwrap();
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]); // WASM magic
    }

    #[test]
    fn test_compile_named() {
        let (name, wasm) = compile_named("contract MyToken {}").unwrap();
        assert_eq!(name, "MyToken");
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_compile_full_location_contract() {
        let src = r#"
            contract LocationTrigger {
                let triggerLat: f32 = 0.0;
                let triggerLon: f32 = 0.0;
                let radius: f32 = 100.0;

                function configure(lat: f32, lon: f32, r: f32): void {
                    triggerLat = lat;
                    triggerLon = lon;
                    radius = r;
                }

                function check(): bool {
                    let loc = @location();
                    let dlat = loc.lat - triggerLat;
                    let dlon = loc.lon - triggerLon;
                    let dist = dlat * dlat + dlon * dlon;
                    if (dist < radius * radius) {
                        emit("triggered", "User entered zone");
                        return true;
                    }
                    return false;
                }
            }
        "#;
        let (name, wasm) = compile_named(src).unwrap();
        assert_eq!(name, "LocationTrigger");
        assert!(wasm.len() > 100); // Non-trivial contract
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_compile_proximity_escrow() {
        let src = r#"
            contract ProximityEscrow {
                let minPeers: i32 = 5;
                let escrowAmount: i64 = 0;
                let isLocked: bool = true;

                function deposit(amount: i64): void {
                    escrowAmount = amount;
                    isLocked = true;
                }

                function tryRelease(): bool {
                    let peers = @proximity();
                    let score = @presence();
                    if (peers >= minPeers && score >= 60) {
                        isLocked = false;
                        emit("released", "Escrow conditions met");
                        return true;
                    }
                    return false;
                }
            }
        "#;
        let wasm = compile(src).unwrap();
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_compile_presence_verification() {
        let src = r#"
            contract PresenceVerifier {
                const minScore: i32 = 70;

                function verify(): bool {
                    let score = @presence();
                    if (score >= minScore) {
                        return true;
                    }
                    return false;
                }

                function getScore(): i32 {
                    return @presence();
                }
            }
        "#;
        let wasm = compile(src).unwrap();
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_compile_sensor_oracle() {
        let src = r#"
            contract WeatherOracle {
                let lastPressure: f64 = 0.0;
                let lastLight: f64 = 0.0;

                function readWeather(): void {
                    lastPressure = @sensor(0);
                    lastLight = @sensor(1);
                }

                function getPressure(): f64 {
                    return lastPressure;
                }
            }
        "#;
        let wasm = compile(src).unwrap();
        assert_eq!(&wasm[0..4], &[0x00, 0x61, 0x73, 0x6D]);
    }

    #[test]
    fn test_compile_error_unknown_builtin() {
        let result = compile("contract C { function f(): void { let x = @unknown(); } }");
        assert!(result.is_err());
    }

    #[test]
    fn test_compile_error_syntax() {
        let result = compile("contract { }"); // missing name
        assert!(result.is_err());
    }
}
