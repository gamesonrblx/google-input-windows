//! Which threads currently have the 粵拼 language profile active.
//! The tray hook reads this and lets those keystrokes through to the text service.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

const SLOTS: usize = 16;

#[repr(C)]
struct Slot {
    pid: u32,
    tid: u32,
    on: u32,
}

fn slots() -> *mut Slot {
    static VIEW: AtomicPtr<Slot> = AtomicPtr::new(null_mut());
    let existing = VIEW.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let created = unsafe { map() };
    if created.is_null() {
        return null_mut();
    }
    match VIEW.compare_exchange(null_mut(), created, Ordering::Release, Ordering::Acquire) {
        Ok(_) => created,
        Err(winner) => winner,
    }
}

unsafe fn map() -> *mut Slot {
    let name: Vec<u16> = "Local\\GoogleInputJyutping\0".encode_utf16().collect();
    let handle = CreateFileMappingW(
        INVALID_HANDLE_VALUE,
        null(),
        PAGE_READWRITE,
        0,
        (std::mem::size_of::<Slot>() * SLOTS) as u32,
        name.as_ptr(),
    );
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return null_mut();
    }
    let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    // Don't CloseHandle: the mapping has to stay alive for the process.
    KEEP_HANDLE.store(handle, Ordering::Release);
    view.Value as *mut Slot
}

static KEEP_HANDLE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(null_mut());

fn me() -> (u32, u32) {
    unsafe { (GetCurrentProcessId(), GetCurrentThreadId()) }
}

pub fn set_here(on: bool) {
    let base = slots();
    if base.is_null() {
        return;
    }
    let (pid, tid) = me();
    unsafe {
        let slots = std::slice::from_raw_parts_mut(base, SLOTS);
        if let Some(slot) = slots.iter_mut().find(|s| s.pid == pid && s.tid == tid) {
            slot.on = u32::from(on);
            return;
        }
        if on {
            if let Some(slot) = slots.iter_mut().find(|s| s.on == 0) {
                slot.pid = pid;
                slot.tid = tid;
                slot.on = 1;
            }
        }
    }
}

/// The foreground window's thread is inside our text service.
pub fn foreground_is_ours() -> bool {
    let base = slots();
    if base.is_null() {
        return false;
    }
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        let slots = std::slice::from_raw_parts(base, SLOTS);
        slots.iter().any(|s| s.on != 0 && s.pid == pid && s.tid == tid)
    }
}
