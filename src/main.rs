mod hooks;

use std::ffi::CString;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameA, GetModuleHandleA, GetProcAddress, LoadLibraryExA,
    LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows_sys::Win32::System::Environment::{SetEnvironmentVariableA, GetCommandLineA};

/// DedicatedMain has WinMain-like signature.
/// On x86 it's __cdecl (no @N decoration). On x64 there's one convention.
type DedicatedMainFn =
    unsafe extern "C" fn(h_instance: HINSTANCE, h_prev: HINSTANCE, cmd_line: *const u8, show: i32) -> i32;

fn main() {
    // -- 1. Determine game root from our own exe path --
    let game_root = get_game_root();

    // -- 2. Determine architecture and DLL paths --
    let is_x64 = cfg!(target_pointer_width = "64");
    let (bin_subdir, dll_rel) = if is_x64 {
        ("bin\\win64", "bin\\win64\\dedicated.dll")
    } else {
        ("bin", "bin\\dedicated.dll")
    };

    let dll_path = game_root.join(dll_rel);
    if !dll_path.exists() {
        eprintln!("[srcds_console] Error: {} not found", dll_path.display());
        eprintln!("[srcds_console] Make sure this exe is placed in the SRCDS game root.");
        std::process::exit(1);
    }

    // -- 3. Prepend bin directory to PATH --
    let bin_dir = game_root.join(bin_subdir);
    prepend_path(&bin_dir);

    eprintln!("[srcds_console] Loading {} ({})", dll_rel, if is_x64 { "x64" } else { "x86" });

    // -- 4. Load dedicated.dll --
    let dll_path_c = path_to_cstring(&dll_path);
    let h_dll = unsafe {
        LoadLibraryExA(dll_path_c.as_ptr() as *const u8, std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
    };
    if h_dll.is_null() {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        eprintln!("[srcds_console] Failed to load {}: error 0x{:08x}", dll_rel, err);
        print_last_error(err);
        std::process::exit(1);
    }

    eprintln!("[srcds_console] dedicated.dll loaded at 0x{:x}", h_dll as usize);

    // -- 5. Install IAT hooks to suppress status bar --
    let hook_count = unsafe { hooks::install_console_hooks(h_dll as *const u8) };
    eprintln!("[srcds_console] Installed {} console hooks", hook_count);

    // -- 6. Get DedicatedMain export --
    let dedicated_main: DedicatedMainFn = unsafe {
        let proc = GetProcAddress(h_dll, b"DedicatedMain\0".as_ptr());
        if proc.is_none() {
            eprintln!("[srcds_console] Error: DedicatedMain not found in dedicated.dll");
            std::process::exit(1);
        }
        std::mem::transmute(proc.unwrap())
    };

    // -- 7. Prepare arguments and call DedicatedMain --
    let h_instance = unsafe { GetModuleHandleA(ptr::null()) };
    let cmd_line = unsafe { GetCommandLineA() };

    eprintln!("[srcds_console] Calling DedicatedMain...");
    eprintln!();

    let exit_code = unsafe { dedicated_main(h_instance, ptr::null_mut(), cmd_line, 0) };
    std::process::exit(exit_code);
}

fn get_game_root() -> PathBuf {
    let mut buf = [0u8; 260];
    let len = unsafe { GetModuleFileNameA(ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        eprintln!("[srcds_console] Error: GetModuleFileNameA failed");
        std::process::exit(1);
    }
    let path_str = std::str::from_utf8(&buf[..len as usize]).unwrap_or_else(|_| {
        eprintln!("[srcds_console] Error: exe path is not valid UTF-8");
        std::process::exit(1);
    });
    let path = PathBuf::from(path_str);
    path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| {
        eprintln!("[srcds_console] Error: cannot determine game root from exe path");
        std::process::exit(1);
    })
}

fn prepend_path(dir: &std::path::Path) {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{};{}", dir.display(), current_path);
    let key = CString::new("PATH").unwrap();
    let val = CString::new(new_path).unwrap();
    unsafe { SetEnvironmentVariableA(key.as_ptr() as *const u8, val.as_ptr() as *const u8); }
}

fn path_to_cstring(path: &std::path::Path) -> CString {
    let s = path.to_str().unwrap_or_else(|| {
        eprintln!("[srcds_console] Error: path contains non-UTF8 characters");
        std::process::exit(1);
    });
    CString::new(s).unwrap_or_else(|_| {
        eprintln!("[srcds_console] Error: path contains null bytes");
        std::process::exit(1);
    })
}

fn print_last_error(code: u32) {
    match code {
        0x000000C1 => eprintln!("  -> ERROR_BAD_EXE_FORMAT: not a valid Win32 application (architecture mismatch?)"),
        0x0000007E => eprintln!("  -> ERROR_MOD_NOT_FOUND: a dependency DLL was not found. Check that bin\\ directory is correct."),
        0x000000B7 => eprintln!("  -> ERROR_ALREADY_EXISTS"),
        _ => eprintln!("  -> Windows error code 0x{:08x} ({})", code, code),
    }
}