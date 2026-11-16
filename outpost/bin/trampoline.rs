#![no_std]
#![no_main]

use core::ffi::c_void;
use core::panic::PanicInfo;

type Bool = i32;
type Dword = u32;
type Handle = *mut c_void;
type LpcWStr = *const u16;
type LpWStr = *mut u16;

const INFINITE: Dword = 0xFFFF_FFFF;
const MAX_TEXT: Dword = 32_768;

#[repr(C)]
struct StartupInfoW {
	cb: Dword,
	lp_reserved: LpWStr,
	lp_desktop: LpWStr,
	lp_title: LpWStr,
	dw_x: Dword,
	dw_y: Dword,
	dw_x_size: Dword,
	dw_y_size: Dword,
	dw_x_count_chars: Dword,
	dw_y_count_chars: Dword,
	dw_fill_attribute: Dword,
	dw_flags: Dword,
	w_show_window: u16,
	cb_reserved2: u16,
	lp_reserved2: *mut u8,
	h_std_input: Handle,
	h_std_output: Handle,
	h_std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
	h_process: Handle,
	h_thread: Handle,
	dw_process_id: Dword,
	dw_thread_id: Dword,
}

unsafe extern "system" {
	fn ExitProcess(u_exit_code: u32) -> !;
	fn GetEnvironmentVariableW(lp_name: LpcWStr, lp_buffer: LpWStr, n_size: Dword) -> Dword;
	fn SearchPathW(lp_path: LpcWStr, lp_file_name: LpcWStr, lp_extension: LpcWStr, n_buffer_length: Dword, lp_buffer: LpWStr, lp_file_part: *mut LpWStr)
	-> Dword;
	fn GetCommandLineW() -> LpcWStr;
	fn CreateProcessW(
		lp_application_name: LpcWStr, lp_command_line: LpWStr, lp_process_attributes: *mut c_void, lp_thread_attributes: *mut c_void, b_inherit_handles: Bool,
		dw_creation_flags: Dword, lp_environment: *mut c_void, lp_current_directory: LpcWStr, lp_startup_info: *mut StartupInfoW,
		lp_process_information: *mut ProcessInformation,
	) -> Bool;
	fn WaitForSingleObject(h_handle: Handle, dw_milliseconds: Dword) -> Dword;
	fn GetExitCodeProcess(h_process: Handle, lp_exit_code: *mut Dword) -> Bool;
	fn CloseHandle(h_object: Handle) -> Bool;
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
	unsafe { ExitProcess(101) }
}

// crt entry point, returns the child exit code
#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
	unsafe {
		// read path so only path is searched for op.exe
		let path_name = crate::wide!("PATH");
		let mut path_env = [0u16; MAX_TEXT as usize];
		let len = GetEnvironmentVariableW(path_name.as_ptr(), path_env.as_mut_ptr(), MAX_TEXT);
		let path = if len == 0 || len >= MAX_TEXT { core::ptr::null() } else { path_env.as_ptr() };
		// resolve op.exe from path
		let op_name = crate::wide!("op.exe");
		let mut op_path = [0u16; MAX_TEXT as usize];
		let found = SearchPathW(path, op_name.as_ptr(), core::ptr::null(), MAX_TEXT, op_path.as_mut_ptr(), core::ptr::null_mut());
		if found == 0 || found >= MAX_TEXT {
			ExitProcess(127);
		}
		// build op.exe exec --shim -- <original command line>
		let prefix = crate::wide!("op.exe exec --shim -- ");
		let mut command_line = [0u16; MAX_TEXT as usize];
		let mut index = 0;
		for unit in prefix {
			if unit == 0 {
				break;
			}
			command_line[index] = unit;
			index += 1;
		}
		let source = GetCommandLineW();
		let mut terminated = false;
		for offset in 0..(MAX_TEXT as usize).saturating_sub(index) {
			let character = *source.add(offset);
			command_line[index + offset] = character;
			// stop at the null terminator
			if character == 0 {
				terminated = true;
				break;
			}
		}
		if !terminated {
			ExitProcess(127);
		}
		// inherit handles, cwd, and environment
		let mut startup: StartupInfoW = core::mem::zeroed();
		startup.cb = core::mem::size_of::<StartupInfoW>() as Dword;
		let mut process: ProcessInformation = core::mem::zeroed();
		let created = CreateProcessW(
			op_path.as_ptr(),
			command_line.as_mut_ptr(),
			core::ptr::null_mut(),
			core::ptr::null_mut(),
			1,
			0,
			core::ptr::null_mut(),
			core::ptr::null(),
			&mut startup,
			&mut process,
		);
		if created == 0 {
			ExitProcess(127);
		}
		// wait for the child and propagate its exit code
		WaitForSingleObject(process.h_process, INFINITE);
		let mut code: Dword = 127;
		if GetExitCodeProcess(process.h_process, &mut code) == 0 {
			code = 127;
		}
		CloseHandle(process.h_process);
		CloseHandle(process.h_thread);
		ExitProcess(code)
	}
}

// ascii literals only
#[macro_export]
macro_rules! wide {
	($s:literal) => {{
		let mut out = [0u16; $s.len() + 1];
		for (slot, byte) in out.iter_mut().zip($s.bytes()) {
			*slot = byte as u16;
		}
		out
	}};
}
