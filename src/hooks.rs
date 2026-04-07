//! IAT hooking for dedicated.dll console APIs.
//!
//! Walks the loaded DLL's import table and replaces function pointers for
//! console APIs that write the status bar directly to screen coordinates.
//! This suppresses the grey line artifacts while leaving normal log output
//! (WriteConsoleW / WriteFile) untouched.

use std::ffi::CStr;
use std::ptr;

use windows_sys::Win32::Foundation::{BOOL, HANDLE};
use windows_sys::Win32::System::Console::{
    COORD, SMALL_RECT, CONSOLE_SCREEN_BUFFER_INFO,
};
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_READWRITE};

// -- PE structures (minimal, for IAT walking) --

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _pad: [u8; 58],
    e_lfanew: i32,
}

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
    _pad: [u8; 110],
    data_directory: [ImageDataDirectory; 16],
}

#[cfg(target_pointer_width = "32")]
#[repr(C)]
struct ImageOptionalHeader32 {
    _magic: u16,
    _pad: [u8; 94],
    data_directory: [ImageDataDirectory; 16],
}

#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32,
    _time_date_stamp: u32,
    _forwarder_chain: u32,
    name: u32,
    first_thunk: u32,
}

#[repr(C)]
struct ImageImportByName {
    _hint: u16,
    name: [u8; 1],
}

const IMAGE_DIRECTORY_ENTRY_IMPORT: usize = 1;
#[cfg(target_pointer_width = "64")]
const IMAGE_ORDINAL_FLAG: usize = 0x8000_0000_0000_0000;
#[cfg(target_pointer_width = "32")]
const IMAGE_ORDINAL_FLAG: usize = 0x8000_0000;

// -- Hook replacement functions --

use std::sync::atomic::{AtomicUsize, Ordering};

static ORIG_WRITE_CONSOLE_OUTPUT_CHAR: AtomicUsize = AtomicUsize::new(0);
static ORIG_WRITE_CONSOLE_OUTPUT_ATTR: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_CURSOR_POS: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_BUFFER_SIZE: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_WINDOW_INFO: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_BUFFER_INFO: AtomicUsize = AtomicUsize::new(0);
static ORIG_ALLOC_CONSOLE: AtomicUsize = AtomicUsize::new(0);
static ORIG_FREE_CONSOLE: AtomicUsize = AtomicUsize::new(0);

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

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_WriteConsoleOutputCharacterA(
    _console: HANDLE,
    _character: *const u8,
    length: u32,
    _write_coord: COORD,
    chars_written: *mut u32,
) -> BOOL {
    if !chars_written.is_null() {
        *chars_written = length;
    }
    1
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

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_AllocConsole() -> BOOL {
    1
}

#[allow(non_snake_case)]
unsafe extern "system" fn hooked_FreeConsole() -> BOOL {
    1
}

// -- IAT hooking engine --

struct HookEntry {
    func_name: &'static [u8],
    hook_fn: usize,
}

/// Install all console hooks on the given loaded module (dedicated.dll).
/// Returns the number of hooks successfully installed.
pub unsafe fn install_console_hooks(module_base: *const u8) -> usize {
    let hooks: &[HookEntry] = &[
        HookEntry { func_name: b"WriteConsoleOutputCharacterA\0", hook_fn: hooked_WriteConsoleOutputCharacterA as *const () as usize },
        HookEntry { func_name: b"WriteConsoleOutputAttribute\0", hook_fn: hooked_WriteConsoleOutputAttribute as *const () as usize },
        HookEntry { func_name: b"SetConsoleCursorPosition\0", hook_fn: hooked_SetConsoleCursorPosition as *const () as usize },
        HookEntry { func_name: b"SetConsoleScreenBufferSize\0", hook_fn: hooked_SetConsoleScreenBufferSize as *const () as usize },
        HookEntry { func_name: b"SetConsoleWindowInfo\0", hook_fn: hooked_SetConsoleWindowInfo as *const () as usize },
        HookEntry { func_name: b"GetConsoleScreenBufferInfo\0", hook_fn: hooked_GetConsoleScreenBufferInfo as *const () as usize },
        HookEntry { func_name: b"AllocConsole\0", hook_fn: hooked_AllocConsole as *const () as usize },
        HookEntry { func_name: b"FreeConsole\0", hook_fn: hooked_FreeConsole as *const () as usize },
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
    let mut desc = module_base.add(import_dir.virtual_address as usize) as *const ImageImportDescriptor;

    while (*desc).name != 0 {
        let dll_name_ptr = module_base.add((*desc).name as usize) as *const i8;
        let dll_name = CStr::from_ptr(dll_name_ptr);

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
    let lookup_rva = if ilt_rva != 0 { ilt_rva } else { iat_rva };

    let mut count = 0;
    let mut idx = 0usize;
    let ptr_size = std::mem::size_of::<usize>();

    loop {
        let lookup_entry = *(base.add(lookup_rva as usize + idx * ptr_size) as *const usize);
        if lookup_entry == 0 { break; }

        if lookup_entry & IMAGE_ORDINAL_FLAG != 0 {
            idx += 1;
            continue;
        }

        let import_by_name = base.add(lookup_entry & !IMAGE_ORDINAL_FLAG) as *const ImageImportByName;
        let func_name = CStr::from_ptr((*import_by_name).name.as_ptr() as *const i8);

        for hook in hooks {
            let hook_name = CStr::from_ptr(hook.func_name.as_ptr() as *const i8);
            if func_name == hook_name {
                let iat_slot = base.add(iat_rva as usize + idx * ptr_size) as *mut usize;

                let mut old_protect: u32 = 0;
                if VirtualProtect(iat_slot as *const _, ptr_size, PAGE_READWRITE, &mut old_protect) != 0 {
                    let original = ptr::read_volatile(iat_slot);
                    save_original(func_name, original);
                    ptr::write_volatile(iat_slot, hook.hook_fn);
                    VirtualProtect(iat_slot as *const _, ptr_size, old_protect, &mut old_protect);
                    count += 1;
                } else {
                    eprintln!("[hooks] VirtualProtect failed for {}", func_name.to_str().unwrap_or("?"));
                }
                break;
            }
        }

        idx += 1;
    }

    count
}