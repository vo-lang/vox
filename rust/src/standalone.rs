//! Standalone C-ABI WASM exports for vox.
//!
//! Each Vo extern is exported as:
//!   `fn <name>(input_ptr, input_len, out_len_ptr) -> output_ptr`
//!
//! Host operations (compile, run) are declared as raw WASM imports under
//! the `"env"` namespace.  The host provides them via importObject at
//! instantiation time and delegates to the studio WASM compiler/runtime.
//!
//! Module handles: compiled bytecodes are stored in a local handle table.
//! Compile* functions return a handle ID; Run*/Free take a handle ID.
//! This keeps the host side stateless for vox.

use std::sync::Mutex;

// ── Tag constants (mirrors ext_bridge.rs) ────────────────────────────────────

const TAG_VALUE: u8 = 0xE2;
const TAG_BYTES: u8 = 0xE3;

// ── Host imports ─────────────────────────────────────────────────────────────

extern "C" {
    // Compile a file/dir at `path`.  Returns serialised bytecode.
    // On error, returns error string with `*ok = 0`.
    // On success, returns bytecode bytes with `*ok = 1`.
    fn host_compile(path_ptr: *const u8, path_len: u32, ok: *mut u32, out_len: *mut u32) -> *mut u8;

    // Compile source code string.  Same return convention as host_compile.
    fn host_compile_string(code_ptr: *const u8, code_len: u32, ok: *mut u32, out_len: *mut u32) -> *mut u8;

    // Compile-check source code.  Returns error message (empty = success).
    fn host_compile_check(code_ptr: *const u8, code_len: u32, out_len: *mut u32) -> *mut u8;

    // Run serialised bytecode (VM mode).  Returns 1 on success, 0 on error.
    // On error, error message is written to `err_ptr/err_len`.
    fn host_run_bytecode(bc_ptr: *const u8, bc_len: u32, err_len: *mut u32) -> *mut u8;

    // Run serialised bytecode capturing stdout.  Returns captured output.
    // On error, `*ok = 0` and result is the error message.
    fn host_run_bytecode_capture(bc_ptr: *const u8, bc_len: u32, ok: *mut u32, out_len: *mut u32) -> *mut u8;

    // VFS read/write/stat for bytecode I/O.
    fn host_vfs_read(path_ptr: *const u8, path_len: u32, ok: *mut u32, out_len: *mut u32) -> *mut u8;
    fn host_vfs_write(path_ptr: *const u8, path_len: u32, data_ptr: *const u8, data_len: u32) -> u32;
    fn host_vfs_exists(path_ptr: *const u8, path_len: u32) -> u32;
}

// ── Memory management ────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn vo_alloc(size: u32) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn vo_dealloc(ptr: *mut u8, size: u32) {
    unsafe { drop(Vec::from_raw_parts(ptr, 0, size as usize)) };
}

// ── Input helpers ────────────────────────────────────────────────────────────

fn raw_input<'a>(ptr: *const u8, len: u32) -> &'a [u8] {
    if len == 0 { return &[]; }
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

fn read_value_arg(buf: &[u8], off: usize) -> (u64, usize) {
    let v = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
    (v, off + 8)
}

fn read_bytes_arg(buf: &[u8], off: usize) -> (&[u8], usize) {
    let len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
    (&buf[off + 4..off + 4 + len], off + 4 + len)
}

// ── Output helpers ───────────────────────────────────────────────────────────

fn alloc_output(data: &[u8], out_len: *mut u32) -> *mut u8 {
    unsafe { *out_len = data.len() as u32; }
    let ptr = vo_alloc(data.len() as u32);
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()); }
    ptr
}

fn output_empty(out_len: *mut u32) -> *mut u8 {
    alloc_output(&[], out_len)
}

fn output_tag_value(v: u64, out_len: *mut u32) -> *mut u8 {
    let mut buf = [0u8; 9];
    buf[0] = TAG_VALUE;
    buf[1..9].copy_from_slice(&v.to_le_bytes());
    alloc_output(&buf, out_len)
}

fn output_tag_bytes(data: &[u8], out_len: *mut u32) -> *mut u8 {
    let mut buf = Vec::with_capacity(5 + data.len());
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    alloc_output(&buf, out_len)
}

/// TAG_VALUE(handle) + TAG_BYTES(b"") for (Module, nil_error)
fn output_handle_ok(handle: i64, out_len: *mut u32) -> *mut u8 {
    // [TAG_VALUE, handle:8] [TAG_BYTES, 0:4]
    let mut buf = [0u8; 9 + 5];
    buf[0] = TAG_VALUE;
    buf[1..9].copy_from_slice(&(handle as u64).to_le_bytes());
    buf[9] = TAG_BYTES;
    // buf[10..14] already zero = empty error string
    alloc_output(&buf, out_len)
}

/// TAG_VALUE(0/nil) + TAG_BYTES(err_msg) for (nil_handle, error)
fn output_handle_err(err: &[u8], out_len: *mut u32) -> *mut u8 {
    let mut buf = Vec::with_capacity(9 + 5 + err.len());
    buf.push(TAG_VALUE);
    buf.extend_from_slice(&0u64.to_le_bytes()); // nil handle
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(err.len() as u32).to_le_bytes());
    buf.extend_from_slice(err);
    alloc_output(&buf, out_len)
}

/// TAG_BYTES(data) + TAG_BYTES(b"") for (string/bytes, nil_error)
fn output_str_ok(s: &[u8], out_len: *mut u32) -> *mut u8 {
    let mut buf = Vec::with_capacity(5 + s.len() + 5);
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s);
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&0u32.to_le_bytes()); // nil error
    alloc_output(&buf, out_len)
}

/// TAG_BYTES(b"") + TAG_BYTES(err_msg) for ("", error)
fn output_str_err(err: &[u8], out_len: *mut u32) -> *mut u8 {
    let mut buf = Vec::with_capacity(5 + 5 + err.len());
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&0u32.to_le_bytes()); // empty string
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(err.len() as u32).to_le_bytes());
    buf.extend_from_slice(err);
    alloc_output(&buf, out_len)
}

/// TAG_BYTES(b"") for nil_error
fn output_nil_error(out_len: *mut u32) -> *mut u8 {
    let buf = [TAG_BYTES, 0, 0, 0, 0]; // empty error
    alloc_output(&buf, out_len)
}

/// TAG_BYTES(err_msg) for error
fn output_error(err: &[u8], out_len: *mut u32) -> *mut u8 {
    let mut buf = Vec::with_capacity(5 + err.len());
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(err.len() as u32).to_le_bytes());
    buf.extend_from_slice(err);
    alloc_output(&buf, out_len)
}

// ── Module handle storage ────────────────────────────────────────────────────

static MODULES: Mutex<Vec<Option<Vec<u8>>>> = Mutex::new(Vec::new());

fn store_module(bytecode: Vec<u8>) -> i64 {
    let mut modules = MODULES.lock().unwrap();
    for (i, slot) in modules.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(bytecode);
            return i as i64;
        }
    }
    let id = modules.len();
    modules.push(Some(bytecode));
    id as i64
}

fn get_module(id: i64) -> Option<Vec<u8>> {
    let modules = MODULES.lock().unwrap();
    modules.get(id as usize).and_then(|s| s.clone())
}

fn free_module(id: i64) {
    let mut modules = MODULES.lock().unwrap();
    if let Some(slot) = modules.get_mut(id as usize) {
        *slot = None;
    }
}

// ── Host call helpers ────────────────────────────────────────────────────────

/// Call host_compile / host_compile_string.
/// Returns (bytecode, None) on success or (empty, Some(err)) on failure.
unsafe fn call_host_compile_path(path: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let mut ok: u32 = 0;
    let mut out_len: u32 = 0;
    let ptr = host_compile(path.as_ptr(), path.len() as u32, &mut ok, &mut out_len);
    let data = if out_len > 0 && !ptr.is_null() {
        let v = std::slice::from_raw_parts(ptr, out_len as usize).to_vec();
        vo_dealloc(ptr, out_len);
        v
    } else {
        Vec::new()
    };
    if ok != 0 { Ok(data) } else { Err(data) }
}

unsafe fn call_host_compile_string(code: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let mut ok: u32 = 0;
    let mut out_len: u32 = 0;
    let ptr = host_compile_string(code.as_ptr(), code.len() as u32, &mut ok, &mut out_len);
    let data = if out_len > 0 && !ptr.is_null() {
        let v = std::slice::from_raw_parts(ptr, out_len as usize).to_vec();
        vo_dealloc(ptr, out_len);
        v
    } else {
        Vec::new()
    };
    if ok != 0 { Ok(data) } else { Err(data) }
}

unsafe fn call_host_run(bytecode: &[u8]) -> Result<(), Vec<u8>> {
    let mut err_len: u32 = 0;
    let ptr = host_run_bytecode(bytecode.as_ptr(), bytecode.len() as u32, &mut err_len);
    if err_len == 0 || ptr.is_null() {
        Ok(())
    } else {
        let err = std::slice::from_raw_parts(ptr, err_len as usize).to_vec();
        vo_dealloc(ptr, err_len);
        Err(err)
    }
}

unsafe fn call_host_run_capture(bytecode: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let mut ok: u32 = 0;
    let mut out_len: u32 = 0;
    let ptr = host_run_bytecode_capture(bytecode.as_ptr(), bytecode.len() as u32, &mut ok, &mut out_len);
    let data = if out_len > 0 && !ptr.is_null() {
        let v = std::slice::from_raw_parts(ptr, out_len as usize).to_vec();
        vo_dealloc(ptr, out_len);
        v
    } else {
        Vec::new()
    };
    if ok != 0 { Ok(data) } else { Err(data) }
}

// ── Extern exports ───────────────────────────────────────────────────────────

// CompileFile(path string) (Module, error)
#[no_mangle]
pub extern "C" fn CompileFile(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (path, _) = read_bytes_arg(input, 0);
    match unsafe { call_host_compile_path(path) } {
        Ok(bytecode) => output_handle_ok(store_module(bytecode), out_len),
        Err(e) => output_handle_err(&e, out_len),
    }
}

// CompileDir(path string) (Module, error)
#[no_mangle]
pub extern "C" fn CompileDir(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    // host_compile handles both files and dirs
    CompileFile(ptr, len, out_len)
}

// CompileString(code string) (Module, error)
#[no_mangle]
pub extern "C" fn CompileString(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (code, _) = read_bytes_arg(input, 0);
    match unsafe { call_host_compile_string(code) } {
        Ok(bytecode) => output_handle_ok(store_module(bytecode), out_len),
        Err(e) => output_handle_err(&e, out_len),
    }
}

// Run(m Module) error
#[no_mangle]
pub extern "C" fn Run(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (handle_id, _) = read_value_arg(input, 0);
    let bytecode = match get_module(handle_id as i64) {
        Some(bc) => bc,
        None => return output_error(b"invalid module handle", out_len),
    };
    match unsafe { call_host_run(&bytecode) } {
        Ok(()) => output_nil_error(out_len),
        Err(e) => output_error(&e, out_len),
    }
}

// RunJit(m Module) error
// In WASM host, JIT is not available — falls back to VM.
#[no_mangle]
pub extern "C" fn RunJit(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    Run(ptr, len, out_len)
}

// RunCapture(m Module) (string, error)
#[no_mangle]
pub extern "C" fn RunCapture(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (handle_id, _) = read_value_arg(input, 0);
    let bytecode = match get_module(handle_id as i64) {
        Some(bc) => bc,
        None => return output_str_err(b"invalid module handle", out_len),
    };
    match unsafe { call_host_run_capture(&bytecode) } {
        Ok(output) => output_str_ok(&output, out_len),
        Err(e) => output_str_err(&e, out_len),
    }
}

// RunJitCapture(m Module) (string, error)
#[no_mangle]
pub extern "C" fn RunJitCapture(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    RunCapture(ptr, len, out_len)
}

// RunFile(path string) error
#[no_mangle]
pub extern "C" fn RunFile(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (path, _) = read_bytes_arg(input, 0);
    let bytecode = match unsafe { call_host_compile_path(path) } {
        Ok(bc) => bc,
        Err(e) => return output_error(&e, out_len),
    };
    match unsafe { call_host_run(&bytecode) } {
        Ok(()) => output_nil_error(out_len),
        Err(e) => output_error(&e, out_len),
    }
}

// RunFileJit(path string) error
#[no_mangle]
pub extern "C" fn RunFileJit(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    RunFile(ptr, len, out_len)
}

// Free(m Module)
#[no_mangle]
pub extern "C" fn Free(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (handle_id, _) = read_value_arg(input, 0);
    free_module(handle_id as i64);
    output_empty(out_len)
}

// Name(m Module) string
#[no_mangle]
pub extern "C" fn Name(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    // Module name is not available in standalone mode (bytecode is opaque)
    output_tag_bytes(b"", out_len)
}

// FormatBytecode(m Module) string
#[no_mangle]
pub extern "C" fn FormatBytecode(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    // Bytecode formatting not available in standalone mode
    output_tag_bytes(b"bytecode formatting not available in web mode", out_len)
}

// ParseFile(path string) (AstNode, error)
#[no_mangle]
pub extern "C" fn ParseFile(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_handle_err(b"AST parsing not available in web mode", out_len)
}

// ParseString(code string) (AstNode, error)
#[no_mangle]
pub extern "C" fn ParseString(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_handle_err(b"AST parsing not available in web mode", out_len)
}

// PrintAst(node AstNode) string
#[no_mangle]
pub extern "C" fn PrintAst(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_tag_bytes(b"", out_len)
}

// FreeAst(node AstNode)
#[no_mangle]
pub extern "C" fn FreeAst(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_empty(out_len)
}

// SaveBytecodeText(m Module, path string) error
#[no_mangle]
pub extern "C" fn SaveBytecodeText(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_error(b"bytecode text I/O not available in web mode", out_len)
}

// LoadBytecodeText(path string) (Module, error)
#[no_mangle]
pub extern "C" fn LoadBytecodeText(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_handle_err(b"bytecode text I/O not available in web mode", out_len)
}

// SaveBytecodeBinary(m Module, path string) error
#[no_mangle]
pub extern "C" fn SaveBytecodeBinary(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (handle_id, off) = read_value_arg(input, 0);
    let (path, _) = read_bytes_arg(input, off);
    let bytecode = match get_module(handle_id as i64) {
        Some(bc) => bc,
        None => return output_error(b"invalid module handle", out_len),
    };
    let ok = unsafe { host_vfs_write(path.as_ptr(), path.len() as u32, bytecode.as_ptr(), bytecode.len() as u32) };
    if ok != 0 {
        output_nil_error(out_len)
    } else {
        output_error(b"failed to write bytecode", out_len)
    }
}

// LoadBytecodeBinary(path string) (Module, error)
#[no_mangle]
pub extern "C" fn LoadBytecodeBinary(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (path, _) = read_bytes_arg(input, 0);
    let mut ok: u32 = 0;
    let mut data_len: u32 = 0;
    let data_ptr = unsafe { host_vfs_read(path.as_ptr(), path.len() as u32, &mut ok, &mut data_len) };
    if ok == 0 || data_ptr.is_null() || data_len == 0 {
        return output_handle_err(b"failed to read bytecode file", out_len);
    }
    let bytecode = unsafe {
        let v = std::slice::from_raw_parts(data_ptr, data_len as usize).to_vec();
        vo_dealloc(data_ptr, data_len);
        v
    };
    output_handle_ok(store_module(bytecode), out_len)
}

// RunGui(m Module) ([]byte, error)
#[no_mangle]
pub extern "C" fn RunGui(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    // RunGui uses a different path in web mode (runGuiEntry in studio WASM)
    output_str_err(b"RunGui not supported via vox in web mode", out_len)
}

// SendGuiEvent(m Module, handlerId int, payload string) ([]byte, error)
#[no_mangle]
pub extern "C" fn SendGuiEvent(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_str_err(b"SendGuiEvent not supported via vox in web mode", out_len)
}

// StopGui(m Module)
#[no_mangle]
pub extern "C" fn StopGui(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_empty(out_len)
}

// CompileCheck(code string) (string, error)
#[no_mangle]
pub extern "C" fn CompileCheck(ptr: *const u8, len: u32, out_len: *mut u32) -> *mut u8 {
    let input = raw_input(ptr, len);
    let (code, _) = read_bytes_arg(input, 0);
    let mut result_len: u32 = 0;
    let result_ptr = unsafe { host_compile_check(code.as_ptr(), code.len() as u32, &mut result_len) };
    let msg = if result_len > 0 && !result_ptr.is_null() {
        let v = unsafe { std::slice::from_raw_parts(result_ptr, result_len as usize).to_vec() };
        unsafe { vo_dealloc(result_ptr, result_len); }
        v
    } else {
        Vec::new()
    };
    // CompileCheck returns (errMsg, nil_error): errMsg is empty on success
    output_str_ok(&msg, out_len)
}

// InitProject(dir string, modName string) (string, error)
#[no_mangle]
pub extern "C" fn InitProject(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    // Project scaffolding not available in web mode (uses real FS)
    output_str_err(b"InitProject not available in web mode", out_len)
}

// InitFile(path string) error
#[no_mangle]
pub extern "C" fn InitFile(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_error(b"InitFile not available in web mode", out_len)
}

// Get(spec string) (string, error)
#[no_mangle]
pub extern "C" fn Get(_ptr: *const u8, _len: u32, out_len: *mut u32) -> *mut u8 {
    output_str_err(b"vo get is not available in web mode", out_len)
}
