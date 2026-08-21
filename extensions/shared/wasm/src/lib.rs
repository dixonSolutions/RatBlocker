//! WebAssembly binding for the RatBlocker core.
//!
//! The ABI is deliberately hand-written rather than generated: it is small
//! enough that the JavaScript glue in `extensions/shared/src/wasm.ts` fits on a
//! page, and it avoids a toolchain dependency whose version must stay in step
//! with the crate.
//!
//! Calling convention
//! ------------------
//! Strings and buffers cross the boundary as `(pointer, length)` pairs into the
//! module's linear memory. Functions that return a buffer return a packed
//! `u64`: the pointer in the high 32 bits, the length in the low 32 bits. The
//! caller owns every returned buffer and must release it with `rb_dealloc`.

use std::cell::RefCell;
use std::collections::HashMap;

use ratblocker_core::rule_engine::EngineConfig;
use ratblocker_core::{Engine, RequestContext, RuleDatabase};

thread_local! {
    static ENGINES: RefCell<HashMap<u32, Engine>> = RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

/// Pack a pointer and length into the single `u64` the ABI returns.
fn pack(ptr: *mut u8, len: usize) -> u64 {
    ((ptr as u32 as u64) << 32) | (len as u32 as u64)
}

/// Hand a `String` to the caller as an owned buffer.
fn export_string(s: String) -> u64 {
    let mut bytes = s.into_bytes();
    bytes.shrink_to_fit();
    let len = bytes.len();
    let ptr = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    pack(ptr, len)
}

/// # Safety
/// `ptr`/`len` must describe a buffer previously produced by this module or by
/// `rb_alloc`, and must not be used again after this call.
unsafe fn borrow<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    std::slice::from_raw_parts(ptr, len)
}

/// Allocate `len` bytes for the caller to write into.
#[no_mangle]
pub extern "C" fn rb_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Release a buffer obtained from `rb_alloc` or returned by this module.
///
/// # Safety
/// `ptr` and `len` must match a live allocation from this module.
#[no_mangle]
pub unsafe extern "C" fn rb_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

/// The message from the most recent failure. Returns a packed pointer/length.
#[no_mangle]
pub extern "C" fn rb_last_error() -> u64 {
    LAST_ERROR.with(|e| export_string(e.borrow().clone()))
}

/// Core version string, so the extension can assert ABI compatibility.
#[no_mangle]
pub extern "C" fn rb_version() -> u64 {
    export_string(ratblocker_core::CORE_VERSION.to_string())
}

/// Build an engine from a compiled `rules.rbdb`. Returns a handle, or 0.
///
/// # Safety
/// `db_ptr`/`db_len` must describe a readable buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_engine_new(
    db_ptr: *const u8,
    db_len: usize,
    cfg_ptr: *const u8,
    cfg_len: usize,
    user_ptr: *const u8,
    user_len: usize,
) -> u32 {
    let db_bytes = borrow(db_ptr, db_len);
    let db: RuleDatabase = match postcard::from_bytes(db_bytes) {
        Ok(db) => db,
        Err(e) => {
            set_error(format!("rule database is not readable: {e}"));
            return 0;
        }
    };

    let config = match parse_config(borrow(cfg_ptr, cfg_len)) {
        Ok(c) => c,
        Err(e) => {
            set_error(e);
            return 0;
        }
    };

    let user_rules = match std::str::from_utf8(borrow(user_ptr, user_len)) {
        Ok(s) => s,
        Err(_) => {
            set_error("user rules are not valid UTF-8");
            return 0;
        }
    };

    let engine = match Engine::from_database(db, user_rules, config) {
        Ok(e) => e,
        Err(e) => {
            set_error(format!("{e}"));
            return 0;
        }
    };

    let handle = NEXT_HANDLE.with(|h| {
        let mut h = h.borrow_mut();
        let v = *h;
        *h += 1;
        v
    });
    ENGINES.with(|m| m.borrow_mut().insert(handle, engine));
    handle
}

fn parse_config(bytes: &[u8]) -> Result<EngineConfig, String> {
    if bytes.is_empty() {
        return Ok(EngineConfig::default());
    }
    serde_json::from_slice(bytes).map_err(|e| format!("configuration is not valid JSON: {e}"))
}

/// Replace an engine's runtime configuration without rebuilding its indexes.
///
/// # Safety
/// `ptr`/`len` must describe a readable JSON buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_set_config(handle: u32, ptr: *const u8, len: usize) -> i32 {
    let config = match parse_config(borrow(ptr, len)) {
        Ok(c) => c,
        Err(e) => {
            set_error(e);
            return -1;
        }
    };
    ENGINES.with(|m| match m.borrow_mut().get_mut(&handle) {
        Some(engine) => {
            *engine.config_mut() = config;
            0
        }
        None => {
            set_error("no such engine handle");
            -1
        }
    })
}

/// Evaluate one request. Input is a JSON `RequestContext`; output is a JSON
/// `FilterResult`.
///
/// # Safety
/// `ptr`/`len` must describe a readable JSON buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_evaluate(handle: u32, ptr: *const u8, len: usize) -> u64 {
    let ctx: RequestContext = match serde_json::from_slice(borrow(ptr, len)) {
        Ok(c) => c,
        Err(e) => {
            set_error(format!("bad request context: {e}"));
            return export_string("{\"decision\":\"allow\",\"matched_rule_id\":null}".into());
        }
    };
    ENGINES.with(|m| {
        let map = m.borrow();
        match map.get(&handle) {
            Some(engine) => {
                let result = engine.evaluate(&ctx);
                export_string(serde_json::to_string(&result).unwrap_or_default())
            }
            None => {
                set_error("no such engine handle");
                export_string("{\"decision\":\"allow\",\"matched_rule_id\":null}".into())
            }
        }
    })
}

/// Cosmetic selectors for a page URL, as a JSON `CosmeticResponse`.
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_cosmetic(handle: u32, ptr: *const u8, len: usize) -> u64 {
    let url = std::str::from_utf8(borrow(ptr, len)).unwrap_or("");
    ENGINES.with(|m| {
        let map = m.borrow();
        match map.get(&handle) {
            Some(engine) => {
                let r = engine.cosmetic_for(url);
                export_string(serde_json::to_string(&r).unwrap_or_default())
            }
            None => export_string("{\"hide\":[]}".into()),
        }
    })
}

/// A ready-to-inject stylesheet for a page, avoiding a JSON round trip on the
/// hot path of every navigation.
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_cosmetic_css(handle: u32, ptr: *const u8, len: usize) -> u64 {
    let url = std::str::from_utf8(borrow(ptr, len)).unwrap_or("");
    ENGINES.with(|m| {
        let map = m.borrow();
        match map.get(&handle) {
            Some(engine) => export_string(engine.cosmetic_for(url).to_stylesheet()),
            None => export_string(String::new()),
        }
    })
}

/// Counts describing the loaded database, as JSON.
#[no_mangle]
pub extern "C" fn rb_stats(handle: u32) -> u64 {
    ENGINES.with(|m| {
        let map = m.borrow();
        match map.get(&handle) {
            Some(engine) => export_string(
                serde_json::json!({
                    "rules": engine.rule_count(),
                    "load": engine.load_stats(),
                    "sources": engine.sources(),
                    "dropped": engine.dropped_rules().len(),
                })
                .to_string(),
            ),
            None => export_string("{}".into()),
        }
    })
}

/// Compile Adblock-syntax text into Chromium `declarativeNetRequest` rules.
///
/// Returns JSON `{ "rules": [...], "problems": [...] }`. The Chromium
/// extension uses this for the user's own rules, so that user rules go through
/// exactly the same parser and converter as the bundled lists.
///
/// # Safety
/// `ptr`/`len` must describe a readable UTF-8 buffer.
#[no_mangle]
pub unsafe extern "C" fn rb_compile_dnr(ptr: *const u8, len: usize, first_id: u32) -> u64 {
    let text = match std::str::from_utf8(borrow(ptr, len)) {
        Ok(s) => s,
        Err(_) => {
            set_error("rules are not valid UTF-8");
            return export_string("{\"rules\":[],\"problems\":[\"not valid UTF-8\"]}".into());
        }
    };
    let (rules, problems) = ratblocker_core::rule_engine::dnr::compile_text(text, first_id);
    export_string(
        serde_json::json!({ "rules": rules, "problems": problems }).to_string(),
    )
}

/// Release an engine and its indexes.
#[no_mangle]
pub extern "C" fn rb_engine_free(handle: u32) {
    ENGINES.with(|m| m.borrow_mut().remove(&handle));
}
