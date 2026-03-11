//! Vox - compile, run, and inspect Vo programs.
//!
//! This crate provides:
//! - Re-exports of vo-engine (compile, run, etc.)
//! - AST parsing and printing
//! - Bytecode formatting
//! - FFI bindings for the vox package in Vo
//!
//! When built with `--features wasm-standalone`, produces a minimal C-ABI
//! WASM module that delegates all operations to the host via imports.

#[cfg(not(feature = "wasm-standalone"))]
mod ffi;
#[cfg(not(feature = "wasm-standalone"))]
mod printer;
#[cfg(not(feature = "wasm-standalone"))]
mod format;
#[cfg(not(feature = "wasm-standalone"))]
pub mod gui;

#[cfg(feature = "wasm-standalone")]
mod standalone;

#[cfg(not(feature = "wasm-standalone"))]
/// Force link this crate's FFI functions.
/// Call this from your main() to ensure linkme symbols are included.
pub fn ensure_linked() {
    // Reference vo_ext_get_entries to force linker to include FFI symbols
    extern "C" {
        fn vo_ext_get_entries() -> vo_ext::ExtensionTable;
    }
    // Use black_box to prevent optimization
    let _ = std::hint::black_box(unsafe { vo_ext_get_entries() });
}

#[cfg(not(feature = "wasm-standalone"))]
pub use vo_engine::{compile, compile_prepared, compile_with_cache, compile_string, compile_with_auto_install, prepare_with_auto_install, CompileError, CompileOutput};
#[cfg(not(feature = "wasm-standalone"))]
pub use vo_engine::{run, run_with_output, RunMode, RunError, RuntimeError, RuntimeErrorKind};
#[cfg(not(feature = "wasm-standalone"))]
pub use vo_engine::Module;

#[cfg(not(feature = "wasm-standalone"))]
pub use printer::AstPrinter;
#[cfg(not(feature = "wasm-standalone"))]
pub use format::{format_text, parse_text};
