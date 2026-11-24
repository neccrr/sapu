//! Generates the Swift bindings: `cargo run -p sapu-ffi --bin uniffi-bindgen -- generate …`
//! (see macos/build-shell.sh).

fn main() {
    uniffi::uniffi_bindgen_main()
}
