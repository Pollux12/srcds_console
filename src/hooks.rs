//! IAT hooking for dedicated.dll console APIs.
//!
//! Walks the loaded DLL's import table and replaces function pointers for
//! console APIs that write the status bar directly to screen coordinates.
//! Captures status text and renders it as a persistent bottom bar using
//! ANSI scroll regions — live status info without grey line artifacts.

use std::ffi::CStr;
use std::ptr;

use windows_sys::Win32::Foundation::{BOOL, HANDLE};
use windows_sys::Win32::System::Console::{
    COORD, SMALL_RECT, CONSOLE_SCREEN_BUFFER_INFO,
    GetStdHandle, WriteConsoleA, GetConsoleMode, SetConsoleMode,
};
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_READWRITE};

const STD_OUTPUT_HANDLE_ID: u32 = 0xFFFF_FFF5; // STD_OUTPUT_HANDLE = (DWORD)-11
const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

// ── PE structures (minimal, for IAT walking) ────────────────────────────

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _pad: [u8; 58],
    e_lfanew: i32,
}

// We use the 64-bit NT headers on x64 and 32-bit on x86.
// IMAGE_OPTIONAL_HEADER differs, but the import directory offset is derived
// from the OptionalHeader.DataDirectory array regardless.
#[cfg(target_pointer_width = "64")]
#[repr(C)]
struct ImageNtHeaders {
    signature: u32,
    file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader64,
}

#[cfg(target_pointer_width = "32")]
#[repr(C)]
struct ImageNtHeaders {
    signature: u32,
    file_header: ImageFileHeader,
    optional_header: ImageOptionalHeader32,
}

#[repr(C)]
struct ImageFileHeader {
    _machine: u16,
    _number_of_sections: u16,
    _time_date_stamp: u32,
    _pointer_to_symbol_table: u32,
    _number_of_symbols: u32,
    _size_of_optional_header: u16,
    _characteristics: u16,
}

#[repr(C)]
struct ImageDataDirectory {
    virtual_address: u32,
    _size: u32,
}

#[cfg(target_pointer_width = "64")]
#[repr(C)]
struct ImageOptionalHeader64 {
    _magic: u16,
    _pad: [u8; 110], // skip to DataDirectory (offset 112 - 2 for magic)
    data_directory: [ImageDataDirectory; 16],
}

#[cfg(target_pointer_width = "32")]
#[repr(C)]
struct ImageOptionalHeader32 {
    _magic: u16,
    _pad: [u8; 94], // skip to DataDirectory (offset 96 - 2 for magic)
    data_directory: [ImageDataDirectory; 16],
}

#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32, // ILT RVA (OriginalFirstThunk)
    _time_date_stamp: u32,
    _forwarder_chain: u32,
    name: u32,                 // DLL name RVA
    first_thunk: u32,          // IAT RVA
}

#[repr(C)]
struct ImageImportByName {
    _hint: u16,
    name: [u8; 1], // variable-length, null-terminated
}

const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;
#[cfg(target_pointer_width = "64")]
const IMAGE_ORDINAL_FLAG: usize = 0x8000_0000_0000_0000;
#[cfg(target_pointer_width = "32")]
const IMAGE_ORDINAL_FLAG: usize = 0x8000_0000;

// ── Hook replacement functions ──────────────────────────────────────────
//
// These have the exact same signature as the Windows API functions they replace.
// On x64 Windows the calling convention is uniform; on x86 the kernel32 exports
// are __stdcall, which maps to `extern "system"` in Rust.
//
// Original function pointers are saved so we can call through if needed.

use std::sync::atomic::{AtomicUsize, AtomicU32, Ordering};

static ORIG_WRITE_CONSOLE_OUTPUT_CHAR: AtomicUsize = AtomicUsize::new(0);
static ORIG_WRITE_CONSOLE_OUTPUT_ATTR: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_CURSOR_POS: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_BUFFER_SIZE: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_WINDOW_INFO: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_BUFFER_INFO: AtomicUsize = AtomicUsize::new(0);
static ORIG_ALLOC_CONSOLE: AtomicUsize = AtomicUsize::new(0);
static ORIG_FREE_CONSOLE: AtomicUsize = AtomicUsize::new(0);

// ── Status bar capture ──────────────────────────────────────────────────
//
// Whether the status bar is enabled (can be disabled via SRCDS_NO_STATUS=1)
static STATUS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Call once at startup to check if status bar should be disabled.
pub fn init_status_config() {
    if let Ok(val) = std::env::var("SRCDS_NO_STATUS") {
        if val == "1" || val.eq_ignore_ascii_case("true") {
            STATUS_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
            eprintln!("[srcds_console] Status bar disabled (SRCDS_NO_STATUS={val})");
        }
    }
}

// SRCDS writes its status bar (FPS, map, players, etc.) using
// WriteConsoleOutputCharacterA at specific screen coordinates.
// We capture this text into a buffer and display it in the terminal title.

const STATUS_MAX_ROWS: usize = 8;
const STATUS_MAX_COLS: usize = 256;

// These statics are only accessed from dedicated.dll's main thread, which
// calls the hooked console APIs sequentially. No synchronization needed.
static mut STATUS_BUF: [[u8; STATUS_MAX_COLS]; STATUS_MAX_ROWS] =
    [[b' '; STATUS_MAX_COLS]; STATUS_MAX_ROWS];
static mut STATUS_LEN: [usize; STATUS_MAX_ROWS] = [0; STATUS_MAX_ROWS];

// Rate-limit title updates — update every N writes to avoid flicker
static WRITE_COUNTER: AtomicU32 = AtomicU32::new(0);
const TITLE_UPDATE_INTERVAL: u32 = 5;

/// Capture status text and update the terminal title bar.
unsafe fn capture_status_and_update_title(
    text: *const u8,
    length: u32,
    coord: COORD,
) {
    let row = coord.Y as usize;
    let col = coord.X as usize;

    if row < STATUS_MAX_ROWS && length > 0 {
        let src = std::slice::from_raw_parts(text, length as usize);
        for (i, &b) in src.iter().enumerate() {
            let c = col + i;
            if c < STATUS_MAX_COLS {
                STATUS_BUF[row][c] = b;
            }
        }
        let end = col + length as usize;
        if end > STATUS_LEN[row] {
            STATUS_LEN[row] = end;
        }
    }

    // Rate-limited status update
    if STATUS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        let count = WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
        if count % TITLE_UPDATE_INTERVAL == 0 {
            update_status_line();
        }
    }
}

/// Write raw bytes to the console, bypassing Rust's stdout mutex.
unsafe fn write_console_raw(s: &str) {
    let stdout = GetStdHandle(STD_OUTPUT_HANDLE_ID);
    let mut written: u32 = 0;
    WriteConsoleA(
        stdout,
        s.as_ptr() as *const _,
        s.len() as u32,
        &mut written,
        ptr::null(),
    );
}

/// Enable VT processing and set up the scroll region for the status bar.
/// Call this once after hooks are installed, before DedicatedMain.
pub unsafe fn setup_scroll_region() {
    if !STATUS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) { return; }

    let stdout = GetStdHandle(STD_OUTPUT_HANDLE_ID);

    // Enable VT processing for ANSI escape sequences
    let mut mode: u32 = 0;
    GetConsoleMode(stdout, &mut mode);
    SetConsoleMode(stdout, mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING);

    // Get terminal size
    let orig = ORIG_GET_BUFFER_INFO.load(Ordering::Acquire);
    if orig == 0 { return; }
    let get_info: unsafe extern "system" fn(HANDLE, *mut CONSOLE_SCREEN_BUFFER_INFO) -> BOOL =
        std::mem::transmute(orig);

    let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
    if get_info(stdout, &mut info) == 0 { return; }

    let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
    let cols = (info.srWindow.Right - info.srWindow.Left + 1) as usize;
    if rows < 3 { return; }

    // Set scroll region (all rows except the last), init status line, move cursor to top
    let blank: String = " ".repeat(cols);
    let setup = format!(
        "\x1b[1;{}r\x1b[{};1H\x1b[7m{}\x1b[0m\x1b[1;1H",
        rows - 1, rows, blank
    );
    write_console_raw(&setup);
}

/// Reset the scroll region and move cursor to the bottom. Call before exit.
pub unsafe fn cleanup_scroll_region() {
    if !STATUS_ENABLED.load(std::sync::atomic::Ordering::Relaxed) { return; }
    write_console_raw("\x1b[r\x1b[999;1H\n");
}

// Track last known terminal size for resize detection
static LAST_SCROLL_ROWS: AtomicU32 = AtomicU32::new(0);

/// Render the captured status text as a bottom bar using ANSI escape sequences.
unsafe fn update_status_line() {
    let orig = ORIG_GET_BUFFER_INFO.load(Ordering::Acquire);
    if orig == 0 { return; }

    let get_info: unsafe extern "system" fn(HANDLE, *mut CONSOLE_SCREEN_BUFFER_INFO) -> BOOL =
        std::mem::transmute(orig);

    let stdout = GetStdHandle(STD_OUTPUT_HANDLE_ID);
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
    if get_info(stdout, &mut info) == 0 { return; }

    let cols = (info.srWindow.Right - info.srWindow.Left + 1) as usize;
    let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
    if rows < 3 { return; }

    // Build status text from captured lines
    let mut status = String::with_capacity(cols);
    for row in 0..STATUS_MAX_ROWS {
        let len = STATUS_LEN[row].min(STATUS_MAX_COLS);
        if len == 0 { continue; }
        let line = std::str::from_utf8(&STATUS_BUF[row][..len])
            .unwrap_or("")
            .trim();
        if !line.is_empty() {
            if !status.is_empty() { status.push_str(" \u{2502} "); } // │ separator
            status.push_str(line);
        }
    }

    // Pad to terminal width
    if status.len() < cols {
        status.extend(std::iter::repeat(' ').take(cols - status.len()));
    }
    status.truncate(cols);

    // Only re-set scroll region if terminal size changed (DECSTBM resets cursor to 1,1)
    let prev_rows = LAST_SCROLL_ROWS.load(Ordering::Relaxed);
    if prev_rows != rows as u32 {
        LAST_SCROLL_ROWS.store(rows as u32, Ordering::Relaxed);
        let region = format!("\x1b[1;{}r", rows - 1);
        write_console_raw(&region);
    }

    // Save cursor, move to status row, clear line, write status in reverse video, restore cursor
    let ansi = format!(
        "\x1b7\x1b[{};1H\x1b[2K\x1b[7m{}\x1b[0m\x1b8",
        rows, status
    );
    write_console_raw(&ansi);
}

/// Store original function pointer for a given hook name.
fn save_original(name: &CStr, original: usize) {
    let name = name.to_bytes();
    if name == b"WriteConsoleOutputCharacterA" {
        ORIG_WRITE_CONSOLE_OUTPUT_CHAR.store(original, Ordering::Release);
    } else if name == b"WriteConsoleOutputAttribute" {
        ORIG_WRITE_CONSOLE_OUTPUT_ATTR.store(original, Ordering::Release);
    } else if name == b"SetConsoleCursorPosition" {
        ORIG_SET_CURSOR_POS.store(original, Ordering::Release);
    } else if name == b"SetConsoleScreenBufferSize" {
        ORIG_SET_BUFFER_SIZE.store(original, Ordering::Release);
    } else if name == b"SetConsoleWindowInfo" {
        ORIG_SET_WINDOW_INFO.store(original, Ordering::Release);
    } else if name == b"GetConsoleScreenBufferInfo" {
        ORIG_GET_BUFFER_INFO.store(original, Ordering::Release);
    } else if name == b"AllocConsole" {
        ORIG_ALLOC_CONSOLE.store(original, Ordering::Release);
    } else if name == b"FreeConsole" {
        ORIG_FREE_CONSOLE.store(original, Ordering::Release);
    }
}

// Capture status bar text and display in terminal title instead of
// writing directly to the console buffer (which causes grey lines).

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_WriteConsoleOutputCharacterA(
    _console: HANDLE,
    character: *const u8,
    length: u32,
    write_coord: COORD,
    chars_written: *mut u32,
) -> BOOL {
    capture_status_and_update_title(character, length, write_coord);
    if !chars_written.is_null() {
        *chars_written = length;
    }
    1 // TRUE — pretend we wrote them
}

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_WriteConsoleOutputAttribute(
    _console: HANDLE,
    _attribute: *const u16,
    length: u32,
    _write_coord: COORD,
    attrs_written: *mut u32,
) -> BOOL {
    if !attrs_written.is_null() {
        *attrs_written = length;
    }
    1
}

// No-op cursor moves entirely — prevents the status bar code from
// jumping the cursor to row 0. The save/restore cycle becomes harmless
// because GetConsoleScreenBufferInfo returns the real position but
// the restore is a no-op (cursor never actually moved).
#[allow(non_snake_case)]
unsafe extern "system" fn hooked_SetConsoleCursorPosition(
    _console: HANDLE,
    _cursor_position: COORD,
) -> BOOL {
    1
}

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_SetConsoleScreenBufferSize(
    _console: HANDLE,
    _size: COORD,
) -> BOOL {
    1
}

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_SetConsoleWindowInfo(
    _console: HANDLE,
    _absolute: BOOL,
    _window: *const SMALL_RECT,
) -> BOOL {
    1
}

// Pass through to real function so cursor position queries return truth.
// This ensures save/restore around status bar writes works correctly.
#[allow(non_snake_case)]
unsafe extern "system" fn hooked_GetConsoleScreenBufferInfo(
    console: HANDLE,
    info: *mut CONSOLE_SCREEN_BUFFER_INFO,
) -> BOOL {
    let orig = ORIG_GET_BUFFER_INFO.load(Ordering::Acquire);
    if orig != 0 {
        let f: unsafe extern "system" fn(HANDLE, *mut CONSOLE_SCREEN_BUFFER_INFO) -> BOOL =
            std::mem::transmute(orig);
        f(console, info)
    } else {
        if info.is_null() {
            return 0;
        }
        let p = &mut *info;
        p.dwSize = COORD { X: 80, Y: 25 };
        p.dwCursorPosition = COORD { X: 0, Y: 0 };
        p.wAttributes = 7;
        p.srWindow = SMALL_RECT { Left: 0, Top: 0, Right: 79, Bottom: 24 };
        p.dwMaximumWindowSize = COORD { X: 80, Y: 25 };
        1
    }
}

// Prevent dedicated.dll from detaching our console and creating a new window.
#[allow(non_snake_case)]
unsafe extern "system" fn hooked_AllocConsole() -> BOOL {
    1 // Pretend success — we already have a console
}

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_FreeConsole() -> BOOL {
    1 // Don't detach — keep using the parent terminal
}

// ── IAT hooking engine ──────────────────────────────────────────────────

struct HookEntry {
    func_name: &'static [u8], // null-terminated
    hook_fn: usize,
}

/// Install all console hooks on the given loaded module (dedicated.dll).
/// Returns the number of hooks successfully installed.
pub unsafe fn install_console_hooks(module_base: *const u8) -> usize {
    let hooks: &[HookEntry] = &[
        HookEntry {
            func_name: b"WriteConsoleOutputCharacterA\0",
            hook_fn: hooked_WriteConsoleOutputCharacterA as *const () as usize,
        },
        HookEntry {
            func_name: b"WriteConsoleOutputAttribute\0",
            hook_fn: hooked_WriteConsoleOutputAttribute as *const () as usize,
        },
        HookEntry {
            func_name: b"SetConsoleCursorPosition\0",
            hook_fn: hooked_SetConsoleCursorPosition as *const () as usize,
        },
        HookEntry {
            func_name: b"SetConsoleScreenBufferSize\0",
            hook_fn: hooked_SetConsoleScreenBufferSize as *const () as usize,
        },
        HookEntry {
            func_name: b"SetConsoleWindowInfo\0",
            hook_fn: hooked_SetConsoleWindowInfo as *const () as usize,
        },
        HookEntry {
            func_name: b"GetConsoleScreenBufferInfo\0",
            hook_fn: hooked_GetConsoleScreenBufferInfo as *const () as usize,
        },
        HookEntry {
            func_name: b"AllocConsole\0",
            hook_fn: hooked_AllocConsole as *const () as usize,
        },
        HookEntry {
            func_name: b"FreeConsole\0",
            hook_fn: hooked_FreeConsole as *const () as usize,
        },
    ];

    let dos = module_base as *const ImageDosHeader;
    if (*dos).e_magic != 0x5A4D {
        eprintln!("[hooks] Invalid DOS signature");
        return 0;
    }

    let nt = module_base.offset((*dos).e_lfanew as isize) as *const ImageNtHeaders;
    if (*nt).signature != 0x0000_4550 {
        eprintln!("[hooks] Invalid PE signature");
        return 0;
    }

    let import_dir = &(*nt).optional_header.data_directory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    if import_dir.virtual_address == 0 {
        eprintln!("[hooks] No import directory");
        return 0;
    }

    let mut count = 0usize;
    let mut desc = module_base.add(import_dir.virtual_address as usize)
        as *const ImageImportDescriptor;

    while (*desc).name != 0 {
        let dll_name_ptr = module_base.add((*desc).name as usize) as *const i8;
        let dll_name = CStr::from_ptr(dll_name_ptr);

        // We only hook KERNEL32.dll imports
        if dll_name.to_bytes().eq_ignore_ascii_case(b"KERNEL32.dll") {
            count += patch_iat_entries(module_base, desc, hooks);
        }

        desc = desc.add(1);
    }

    count
}

unsafe fn patch_iat_entries(
    base: *const u8,
    desc: *const ImageImportDescriptor,
    hooks: &[HookEntry],
) -> usize {
    let ilt_rva = (*desc).original_first_thunk;
    let iat_rva = (*desc).first_thunk;

    // Some linkers set OriginalFirstThunk to 0; fall back to FirstThunk
    let lookup_rva = if ilt_rva != 0 { ilt_rva } else { iat_rva };

    let mut count = 0;
    let mut idx = 0usize;
    let ptr_size = std::mem::size_of::<usize>();

    loop {
        let lookup_entry = *(base.add(lookup_rva as usize + idx * ptr_size) as *const usize);
        if lookup_entry == 0 {
            break;
        }

        // Skip ordinal imports
        if lookup_entry & IMAGE_ORDINAL_FLAG != 0 {
            idx += 1;
            continue;
        }

        let import_by_name =
            base.add(lookup_entry & !IMAGE_ORDINAL_FLAG) as *const ImageImportByName;
        let func_name = CStr::from_ptr((*import_by_name).name.as_ptr() as *const i8);

        for hook in hooks {
            let hook_name = CStr::from_ptr(hook.func_name.as_ptr() as *const i8);
            if func_name == hook_name {
                let iat_slot =
                    base.add(iat_rva as usize + idx * ptr_size) as *mut usize;

                let mut old_protect: u32 = 0;
                if VirtualProtect(
                    iat_slot as *const _,
                    ptr_size,
                    PAGE_READWRITE,
                    &mut old_protect,
                ) != 0
                {
                    let original = ptr::read_volatile(iat_slot);
                    save_original(func_name, original);
                    ptr::write_volatile(iat_slot, hook.hook_fn);
                    VirtualProtect(
                        iat_slot as *const _,
                        ptr_size,
                        old_protect,
                        &mut old_protect,
                    );
                    count += 1;
                } else {
                    eprintln!(
                        "[hooks] VirtualProtect failed for {}",
                        func_name.to_str().unwrap_or("?")
                    );
                }
                break;
            }
        }

        idx += 1;
    }

    count
}
