/// UniFFI binding generator binary.
///
/// WHY: UniFFI needs a binary in the consuming crate's context to generate
/// language bindings (Kotlin, Swift). Running `cargo run -p gratia-ffi
/// --bin uniffi-bindgen -- generate ...` invokes this, which delegates to
/// UniFFI's CLI. This ensures the generated bindings match the exact version
/// of UniFFI used by gratia-ffi.
fn main() {
    uniffi::uniffi_bindgen_main()
}
