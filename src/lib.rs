//! Total Commander packer plugin (WCX, x64, Unicode) for Unreal Engine IoStore
//! containers (`.utoc` + `.ucas`). Read-only in phase 1 (SPEC.md).
//!
//! Module map: `read` (open, keys, global lookup, list, extract/test), `archive`
//! (listing model), `info` (the `.utoc` info file), `global`, `profile` (game profiles),
//! `keys` + `dialog` (AES keys), `config`, `log`, `progress`, `win` (Win32 helpers).
//! Every export runs inside [`guard`], so a Rust panic becomes an error popup instead
//! of taking Total Commander down.
#![allow(non_snake_case, clippy::missing_safety_doc)]

pub mod archive;
pub mod busy;
pub mod config;
pub mod dialog;
pub mod ffi;
pub mod global;
pub mod info;
pub mod keys;
pub mod legacy;
pub mod log;
pub mod pack;
pub mod pack_settings;
pub mod validate;
pub mod source_stream;
pub mod profile;
mod progress;
pub mod read;
pub mod view;
pub mod win;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod pack_tests;

use archive::Archive;
use ffi::*;
use std::ffi::{c_char, c_int, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Once;

type Handle = *mut c_void;

// ---------------------------------------------------------------------------
// Infrastructure

/// Runs `f`, turning a panic into an error popup and `fallback`.
fn guard<T>(fallback: T, func: &str, f: impl FnOnce() -> T) -> T {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            dlog!("PANIC in {func}: {msg}");
            show_error(&format!("Internal error in {func}:\n{msg}\n\nThe operation was cancelled."));
            fallback
        }
    }
}

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| {
        if let Some(deps) = config::deps_dir() {
            iostore::set_oodle_search_paths([deps]);
        }
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if log::enabled() {
                log::write(&format!("panic: {info}"));
            } else if cfg!(test) {
                default_hook(info);
            }
        }));
    });
}

/// Re-reads `uetoc.ini` (so DebugLog can be toggled without restarting TC).
fn reload_options() -> config::Options {
    let options = config::load_options();
    let was_enabled = log::enabled();
    log::configure(options.debug_log);
    if options.debug_log && !was_enabled {
        dlog!("plugin: {:?}", win::module_path());
        dlog!("settings dir: {:?}, ini: {:?}", config::settings_dir(), config::ini_path());
        dlog!("options: {options:?}");
        let deps = config::deps_dir();
        let listing = deps
            .as_ref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|it| it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "<missing>".into());
        dlog!("deps dir: {deps:?} contains: [{listing}]");
    }
    for w in &options.warnings {
        dlog!("options: {w}");
    }
    options
}

/// Shows a warning, appending where the debug log went (copying it to the clipboard
/// first if it is in memory).
fn show_warning(text: &str) {
    match log::on_error() {
        Some(note) => win::warn(&format!("{text}\n\n{note}")),
        None => win::warn(text),
    }
}

fn show_error(text: &str) {
    match log::on_error() {
        Some(note) => win::error(&format!("{text}\n\n{note}")),
        None => win::error(text),
    }
}

fn code_name(code: c_int) -> &'static str {
    match code {
        0 => "OK",
        E_END_ARCHIVE => "E_END_ARCHIVE",
        E_NO_MEMORY => "E_NO_MEMORY",
        E_BAD_DATA => "E_BAD_DATA",
        E_BAD_ARCHIVE => "E_BAD_ARCHIVE",
        E_UNKNOWN_FORMAT => "E_UNKNOWN_FORMAT",
        E_EOPEN => "E_EOPEN",
        E_ECREATE => "E_ECREATE",
        E_ECLOSE => "E_ECLOSE",
        E_EREAD => "E_EREAD",
        E_EWRITE => "E_EWRITE",
        E_EABORTED => "E_EABORTED",
        E_NOT_SUPPORTED => "E_NOT_SUPPORTED",
        _ => "?",
    }
}

fn op_name(op: c_int) -> &'static str {
    match op {
        PK_SKIP => "SKIP",
        PK_TEST => "TEST",
        PK_EXTRACT => "EXTRACT",
        _ => "?",
    }
}

unsafe fn archive<'a>(h: Handle) -> Option<&'a mut Archive> {
    unsafe { (h as *mut Archive).as_mut() }
}

fn open_impl(name: Option<String>, mode: c_int, result: &mut c_int) -> Handle {
    init();
    let Some(name) = name else {
        *result = E_EOPEN;
        return std::ptr::null_mut();
    };
    let path = PathBuf::from(name);
    let options = reload_options();
    dlog!("OpenArchive {} mode={}", path.display(), if mode == PK_OM_LIST { "LIST" } else { "EXTRACT" });
    match read::open_archive(&path, &options, mode == PK_OM_LIST) {
        Ok(arc) => {
            *result = 0;
            Box::into_raw(Box::new(arc)) as Handle
        }
        Err(code) => {
            dlog!("  -> {}", code_name(code));
            *result = code;
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Exports: Unicode

#[unsafe(no_mangle)]
pub unsafe extern "system" fn OpenArchiveW(data: *mut OpenArchiveDataW) -> Handle {
    guard(std::ptr::null_mut(), "OpenArchiveW", || {
        let Some(data) = (unsafe { data.as_mut() }) else {
            return std::ptr::null_mut();
        };
        let name = unsafe { win::from_wide_ptr(data.ArcName) };
        open_impl(name, data.OpenMode, &mut data.OpenResult)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ReadHeaderExW(h: Handle, hd: *mut HeaderDataExW) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeaderExW", || {
        let (Some(arc), Some(hd)) = (unsafe { archive(h) }, unsafe { hd.as_mut() }) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = read::next_header(arc) else {
            dlog!("ReadHeader: end of archive after {} entries", arc.items.len());
            return E_END_ARCHIVE;
        };
        unsafe { std::ptr::write_bytes(hd as *mut HeaderDataExW, 0, 1) };
        win::copy_wide(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_wide(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack as u32;
        hd.PackSizeHigh = (out.pack >> 32) as u32;
        hd.UnpSize = out.unpack as u32;
        hd.UnpSizeHigh = (out.unpack >> 32) as u32;
        hd.FileTime = out.time;
        hd.FileAttr = out.attr;
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessFileW(h: Handle, op: c_int, dest_path: *const u16, dest_name: *const u16) -> c_int {
    guard(E_BAD_DATA, "ProcessFileW", || {
        let Some(arc) = (unsafe { archive(h) }) else {
            return E_BAD_ARCHIVE;
        };
        let dest = read::join_dest(unsafe { win::from_wide_ptr(dest_path) }, unsafe { win::from_wide_ptr(dest_name) });
        read::process(arc, op, dest)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn CloseArchive(h: Handle) -> c_int {
    guard(E_ECLOSE, "CloseArchive", || {
        if !h.is_null() {
            let arc = unsafe { Box::from_raw(h as *mut Archive) };
            dlog!("CloseArchive {}", arc.path.display());
            drop(arc);
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn SetProcessDataProcW(_h: Handle, proc_: ProcessDataProcW) {
    progress::set_callback(proc_);
}

#[unsafe(no_mangle)]
pub extern "system" fn SetChangeVolProcW(_h: Handle, _proc: ChangeVolProcW) {}

#[unsafe(no_mangle)]
pub extern "system" fn GetPackerCaps() -> c_int {
    // TC caches caps: the plugin must be reinstalled after a change here.
    PK_CAPS_NEW | PK_CAPS_MODIFY | PK_CAPS_DELETE | PK_CAPS_MULTIPLE | PK_CAPS_OPTIONS | PK_CAPS_BY_CONTENT | PK_CAPS_SEARCHTEXT
}

pub const BACKGROUND_UNPACK: c_int = 1;
pub const BACKGROUND_PACK: c_int = 2;

/// Unpacking and packing are thread-safe: per-handle state, per-thread progress callbacks,
/// locked shared state, one key dialog at a time, a busy guard per container being written.
#[unsafe(no_mangle)]
pub extern "system" fn GetBackgroundFlags() -> c_int {
    BACKGROUND_UNPACK | BACKGROUND_PACK
}

fn describe_list(items: &[String]) -> String {
    items.iter().enumerate().map(|(i, s)| format!("  [{i}] {s:?}")).collect::<Vec<_>>().join("\n")
}

/// Logs and shows a packing error; returns the TC code.
fn report_pack_error(packed: &Path, e: pack::PackError, outcome: &str) -> c_int {
    match e {
        pack::PackError::Aborted => {
            dlog!("  -> cancelled");
            E_EABORTED
        }
        pack::PackError::User(msg) => {
            dlog!("  -> refused: {msg}");
            show_warning(&msg);
            E_EABORTED
        }
        pack::PackError::Failed(msg) => {
            dlog!("  -> failed: {msg}");
            // Our message is more precise than TC's generic one: show it and keep TC quiet.
            show_error(&format!("{}\n\nThe operation failed: {msg}\n{outcome}", packed.display()));
            E_EABORTED
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn PackFilesW(packed_file: *const u16, sub_path: *const u16, src_path: *const u16, add_list: *const u16, flags: c_int) -> c_int {
    guard(E_NOT_SUPPORTED, "PackFilesW", || {
        init();
        let options = reload_options();
        let packed = unsafe { win::from_wide_ptr(packed_file) };
        let sub = unsafe { win::from_wide_ptr(sub_path) };
        let src = unsafe { win::from_wide_ptr(src_path) };
        let items = unsafe { win::from_wide_multi(add_list) };
        dlog!("PackFiles {packed:?} sub={sub:?} src={src:?} flags={flags}\n{}", describe_list(&items));
        let (Some(packed), Some(src)) = (packed, src) else {
            return E_ECREATE;
        };
        let packed = PathBuf::from(packed);
        let started = std::time::Instant::now();
        match pack::pack_files(&packed, sub.as_deref(), Path::new(&src), &items, flags, &options) {
            Ok(st) => {
                dlog!(
                    "  -> OK in {} ms: {} replaced, {} added, {} bytes{}{}{}",
                    started.elapsed().as_millis(),
                    st.replaced,
                    st.added,
                    st.input_bytes,
                    if st.new_container { ", new container" } else { "" },
                    if st.compacted { ", rebuilt" } else { "" },
                    st.pak_written.as_ref().map(|p| format!(", wrote {}", p.display())).unwrap_or_default()
                );
                if flags & pack::PK_PACK_MOVE_FILES != 0 {
                    let srcs: Vec<PathBuf> = items.iter().filter(|i| !i.ends_with('\\')).map(|i| Path::new(&src).join(i.replace(['\\', '/'], std::path::MAIN_SEPARATOR_STR))).collect();
                    pack::delete_sources(&srcs);
                }
                0
            }
            Err(e) => report_pack_error(&packed, e, "The container was left unchanged."),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DeleteFilesW(packed_file: *const u16, delete_list: *const u16) -> c_int {
    guard(E_NOT_SUPPORTED, "DeleteFilesW", || {
        init();
        let options = reload_options();
        let items = unsafe { win::from_wide_multi(delete_list) };
        let Some(packed) = (unsafe { win::from_wide_ptr(packed_file) }) else {
            return E_EOPEN;
        };
        let packed = PathBuf::from(packed);
        dlog!("DeleteFiles {}\n{}", packed.display(), describe_list(&items));
        let started = std::time::Instant::now();
        match pack::delete_files(&packed, &items, &options) {
            Ok(st) => {
                dlog!("  -> OK in {} ms: {} removed{}", started.elapsed().as_millis(), st.removed, if st.compacted { ", rebuilt" } else { "" });
                0
            }
            Err(e) => report_pack_error(&packed, e, "The container was left unchanged."),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn ConfigurePacker(parent: *mut c_void, _dll_instance: *mut c_void) {
    guard((), "ConfigurePacker", || {
        init();
        let ini = config::ini_path().or_else(|| config::settings_dir().map(|d| d.join(config::INI_NAME)));
        let keys = config::settings_dir().map(|d| d.join(keys::KEYS_NAME));
        let show = |p: Option<PathBuf>| p.map(|p| p.display().to_string()).unwrap_or_else(|| "(plugin folder not found)".into());
        let info = format!(
            "Plugin {}. Settings: {}\nAES keys: {}\nOodle library folder: {}",
            env!("CARGO_PKG_VERSION"),
            show(ini),
            show(keys),
            show(config::deps_dir())
        );
        let opts = reload_options();
        let form = pack_settings::PackForm::from_options(&opts.pack);
        let Some(answer) = dialog::configure_pack(parent, &form, &info) else { return };
        if answer == form {
            return;
        }
        // validated by the dialog
        let Ok(pack) = answer.to_options() else { return };
        match config::set_values("Pack", &pack_settings::ini_values(&pack)) {
            Ok(path) => dlog!("ConfigurePacker: [Pack] saved to {}", path.display()),
            Err(e) => win::error(&format!("The settings could not be saved: {e}")),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn CanYouHandleThisFileW(name: *const u16) -> c_int {
    guard(0, "CanYouHandleThisFileW", || unsafe { win::from_wide_ptr(name) }.is_some_and(|n| read::looks_like_container(Path::new(&n))) as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn PackSetDefaultParams(dps: *const PackDefaultParamStruct) {
    guard((), "PackSetDefaultParams", || {
        if let Some(dps) = unsafe { dps.as_ref() } {
            if let Some(ini) = unsafe { win::from_ansi_ptr(dps.DefaultIniName.as_ptr()) } {
                config::set_tc_ini_name(Path::new(&ini));
            }
        }
        init();
    })
}

// ---------------------------------------------------------------------------
// Exports: ANSI fallbacks (64-bit TC uses the W versions when present)

#[unsafe(no_mangle)]
pub unsafe extern "system" fn OpenArchive(data: *mut OpenArchiveData) -> Handle {
    guard(std::ptr::null_mut(), "OpenArchive", || {
        let Some(data) = (unsafe { data.as_mut() }) else {
            return std::ptr::null_mut();
        };
        let name = unsafe { win::from_ansi_ptr(data.ArcName) };
        open_impl(name, data.OpenMode, &mut data.OpenResult)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ReadHeaderEx(h: Handle, hd: *mut HeaderDataEx) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeaderEx", || {
        let (Some(arc), Some(hd)) = (unsafe { archive(h) }, unsafe { hd.as_mut() }) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = read::next_header(arc) else {
            return E_END_ARCHIVE;
        };
        unsafe { std::ptr::write_bytes(hd as *mut HeaderDataEx, 0, 1) };
        win::copy_ansi(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_ansi(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack as u32;
        hd.PackSizeHigh = (out.pack >> 32) as u32;
        hd.UnpSize = out.unpack as u32;
        hd.UnpSizeHigh = (out.unpack >> 32) as u32;
        hd.FileTime = out.time;
        hd.FileAttr = out.attr;
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ReadHeader(h: Handle, hd: *mut HeaderData) -> c_int {
    guard(E_BAD_ARCHIVE, "ReadHeader", || {
        let (Some(arc), Some(hd)) = (unsafe { archive(h) }, unsafe { hd.as_mut() }) else {
            return E_BAD_ARCHIVE;
        };
        let Some(out) = read::next_header(arc) else {
            return E_END_ARCHIVE;
        };
        unsafe { std::ptr::write_bytes(hd as *mut HeaderData, 0, 1) };
        win::copy_ansi(&mut hd.ArcName, &arc.path.to_string_lossy());
        win::copy_ansi(&mut hd.FileName, &out.name);
        hd.PackSize = out.pack.min(i32::MAX as u64) as i32;
        hd.UnpSize = out.unpack.min(i32::MAX as u64) as i32;
        hd.FileTime = out.time;
        hd.FileAttr = out.attr;
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn ProcessFile(h: Handle, op: c_int, dest_path: *const c_char, dest_name: *const c_char) -> c_int {
    guard(E_BAD_DATA, "ProcessFile", || {
        let Some(arc) = (unsafe { archive(h) }) else {
            return E_BAD_ARCHIVE;
        };
        let dest = read::join_dest(unsafe { win::from_ansi_ptr(dest_path) }, unsafe { win::from_ansi_ptr(dest_name) });
        read::process(arc, op, dest)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn SetProcessDataProc(_h: Handle, _proc: ProcessDataProcA) {
    // Progress goes through the W callback only (TC x64 always uses the W exports).
}

#[unsafe(no_mangle)]
pub extern "system" fn SetChangeVolProc(_h: Handle, _proc: ChangeVolProcA) {}

/// TC needs the ANSI entry points to exist before it offers packing; TC x64 then calls the
/// Unicode ones (as tcl-uepak).
#[unsafe(no_mangle)]
pub extern "system" fn PackFiles(_packed_file: *const c_char, _sub_path: *const c_char, _src_path: *const c_char, _add_list: *const c_char, _flags: c_int) -> c_int {
    dlog!("PackFiles (ANSI) called; only the Unicode version is supported");
    E_NOT_SUPPORTED
}

#[unsafe(no_mangle)]
pub extern "system" fn DeleteFiles(_packed_file: *const c_char, _delete_list: *const c_char) -> c_int {
    dlog!("DeleteFiles (ANSI) called; only the Unicode version is supported");
    E_NOT_SUPPORTED
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn CanYouHandleThisFile(name: *const c_char) -> c_int {
    guard(0, "CanYouHandleThisFile", || unsafe { win::from_ansi_ptr(name) }.is_some_and(|n| read::looks_like_container(Path::new(&n))) as c_int)
}
