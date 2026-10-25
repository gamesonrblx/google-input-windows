//! Register 粵拼 as a per-user Windows input method, under Chinese (Hong Kong).
//! No admin: the class and the text-service profile live in HKCU.

use crate::gdi::w;
use crate::ids::{self, guid_braced, DESCRIPTION, LANGID};
use std::fs;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

const CLSID_TF_INPUT_PROCESSOR_PROFILES: GUID = ids::guid(0x33C53A50, 0xF456, 0x4884, [0xB0, 0x49, 0x85, 0xFD, 0x64, 0x3E, 0xCF, 0xED]);
const CLSID_TF_CATEGORY_MGR: GUID = ids::guid(0xA4B544A1, 0x438D, 0x4B41, [0x93, 0x25, 0x86, 0x95, 0x23, 0xE2, 0xD6, 0xC7]);
const IID_PROFILE_MGR: GUID = ids::guid(0x71C6E74C, 0x0F28, 0x11D8, [0xA8, 0x2A, 0x00, 0x06, 0x5B, 0x84, 0x43, 0x5C]);
const IID_CATEGORY_MGR: GUID = ids::guid(0xC3ACEFB5, 0xF69D, 0x4905, [0x93, 0x8F, 0xFC, 0xAD, 0xCF, 0x4B, 0xE8, 0x30]);

/// Keyboard TIP, immersive apps, and the taskbar language switcher.
const CATEGORIES: [GUID; 3] = [
    ids::guid(0x34745C63, 0xB2F0, 0x4784, [0x8B, 0x67, 0x5E, 0x12, 0xC8, 0x70, 0x1A, 0x31]),
    ids::guid(0x13A016DF, 0x560B, 0x46CD, [0x94, 0x7A, 0x4C, 0x3A, 0xF1, 0xE0, 0xE3, 0x5D]),
    ids::guid(0x25504FB4, 0x7BAB, 0x4BC1, [0x9C, 0x69, 0xCF, 0x81, 0x89, 0x0F, 0x0E, 0xF5]),
];

pub fn dll_beside_exe() -> Option<PathBuf> {
    let mut buf = [0u16; 32768];
    let n = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let mut path = PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
    path.set_file_name("jyutping2.dll");
    path.is_file().then_some(path)
}

/// Register the DLL next to this exe and add Chinese (Jyutping) to zh-Hant-HK.
/// Failures are written to the settings folder; the tray app still runs.
pub fn install(dll: &Path) {
    let mut notes = Vec::new();
    match register_com(dll) {
        Ok(()) => notes.push("com class registered".into()),
        Err(e) => notes.push(format!("com class: {e}")),
    }
    match register_profile() {
        Ok(hr) => notes.push(format!("RegisterProfile hr={hr}")),
        Err(e) => notes.push(format!("RegisterProfile: {e}")),
    }
    // HKCU copy as well: RegisterProfile writes HKLM and needs elevation.
    match register_profile_hkcu() {
        Ok(()) => notes.push("hkcu profile written".into()),
        Err(e) => notes.push(format!("hkcu profile: {e}")),
    }
    match ensure_machine(dll) {
        Ok(()) => notes.push("hklm profile present".into()),
        Err(e) => notes.push(format!("hklm profile: {e}")),
    }
    match add_to_language_list() {
        Ok(changed) => notes.push(if changed { "language list updated".into() } else { "language list already had 粵拼".into() }),
        Err(e) => notes.push(format!("language list: {e}")),
    }
    log(&notes.join("\n"));
}

fn log(text: &str) {
    if let Some(dir) = std::env::var_os("APPDATA") {
        let path = PathBuf::from(dir).join("GoogleInputTools").join("register.log");
        let _ = fs::create_dir_all(path.parent().unwrap());
        let _ = fs::write(path, text);
    }
}

fn register_com(dll: &Path) -> Result<(), String> {
    let clsid = guid_braced(&ids::CLSID);
    let key = format!("Software\\Classes\\CLSID\\{clsid}\\InprocServer32");
    let dll = dll.to_string_lossy().to_string();
    unsafe {
        let h = create_key(&key)?;
        set_sz(h, null(), &dll)?;
        set_sz(h, w("ThreadingModel").as_ptr(), "Apartment")?;
        RegCloseKey(h);
        let root = format!("Software\\Classes\\CLSID\\{clsid}");
        let h = create_key(&root)?;
        set_sz(h, null(), DESCRIPTION)?;
        RegCloseKey(h);
    }
    Ok(())
}

fn register_profile() -> Result<i32, String> {
    unsafe {
        let mut mgr = null_mut();
        let hr = CoCreateInstance(&CLSID_TF_INPUT_PROCESSOR_PROFILES, null_mut(), CLSCTX_INPROC_SERVER, &IID_PROFILE_MGR, &mut mgr);
        if hr < 0 {
            return Err(format!("CoCreateInstance {hr:#x}"));
        }
        let hr = register_profile_call(mgr);
        release(mgr);
        if hr < 0 {
            return Err(format!("{hr:#x}"));
        }
        let mut cat = null_mut();
        let hr = CoCreateInstance(&CLSID_TF_CATEGORY_MGR, null_mut(), CLSCTX_INPROC_SERVER, &IID_CATEGORY_MGR, &mut cat);
        if hr < 0 {
            return Ok(hr); // profile exists; categories are also written under HKCU
        }
        for category in &CATEGORIES {
            let _ = register_category_call(cat, category);
        }
        release(cat);
        Ok(0)
    }
}

unsafe fn register_profile_call(this: *mut core::ffi::c_void) -> i32 {
    type Fn = unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const GUID,
        u16,
        *const GUID,
        *const u16,
        u32,
        *const u16,
        u32,
        u32,
        isize,
        u32,
        i32,
        u32,
    ) -> i32;
    let vtbl = *(this as *const *const Fn);
    // IUnknown is 3 methods; RegisterProfile is the 9th method overall (index 8).
    let register = *vtbl.add(8);
    let desc = w(DESCRIPTION);
    register(this, &ids::CLSID, LANGID, &ids::PROFILE, desc.as_ptr(), (desc.len() - 1) as u32, null(), 0, 0, ids::HKL_US, 0, 1, 0)
}

unsafe fn register_category_call(this: *mut core::ffi::c_void, category: &GUID) -> i32 {
    type Fn = unsafe extern "system" fn(*mut core::ffi::c_void, *const GUID, *const GUID, *const GUID) -> i32;
    let vtbl = *(this as *const *const Fn);
    let register = *vtbl.add(3);
    register(this, &ids::CLSID, category, &ids::CLSID)
}

/// HKLM registration. Windows only lists an input method that is registered here.
/// A normal user cannot write it, so the first run asks for consent once.
pub fn install_machine(dll: &Path) -> Result<(), String> {
    register_profile_hive(HKEY_LOCAL_MACHINE, dll)
}

fn ensure_machine(dll: &Path) -> Result<(), String> {
    if machine_present() {
        return Ok(());
    }
    match register_profile_hive(HKEY_LOCAL_MACHINE, dll) {
        Ok(()) => Ok(()),
        Err(e) if e.contains("-> 5") => {
            if elevate_machine_registration() && machine_present() {
                Ok(())
            } else {
                Err("administrator consent was not granted".into())
            }
        }
        Err(e) => Err(e),
    }
}

fn machine_present() -> bool {
    let clsid = guid_braced(&ids::CLSID);
    let profile = guid_braced(&ids::PROFILE);
    let path = format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}\\LanguageProfile\\0x{LANGID:08x}\\{profile}");
    unsafe {
        let mut h = null_mut();
        let name = w(&path);
        let rc = RegOpenKeyExW(HKEY_LOCAL_MACHINE, name.as_ptr(), 0, KEY_READ, &mut h);
        if rc == 0 {
            RegCloseKey(h);
            true
        } else {
            false
        }
    }
}

fn elevate_machine_registration() -> bool {
    let mut buf = [0u16; 32768];
    let n = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return false;
    }
    let exe = w(&String::from_utf16_lossy(&buf[..n as usize]));
    let verb = w("runas");
    let params = w("--register-machine");
    unsafe {
        let mut info: SHELLEXECUTEINFOW = zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = exe.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
            return false;
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        windows_sys::Win32::Foundation::CloseHandle(info.hProcess);
        true
    }
}

fn register_profile_hive(root: HKEY, dll: &Path) -> Result<(), String> {
    let clsid = guid_braced(&ids::CLSID);
    let profile = guid_braced(&ids::PROFILE);
    let dll = dll.to_string_lossy().to_string();
    unsafe {
        let lang = format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}\\LanguageProfile\\0x{LANGID:08x}\\{profile}");
        let h = create_key_at(root, &lang)?;
        set_sz(h, w("Description").as_ptr(), DESCRIPTION)?;
        set_dword(h, w("Enable").as_ptr(), 1)?;
        RegCloseKey(h);
        let class = format!("SOFTWARE\\Classes\\CLSID\\{clsid}");
        let h = create_key_at(root, &class)?;
        set_sz(h, null(), DESCRIPTION)?;
        RegCloseKey(h);
        let server = format!("SOFTWARE\\Classes\\CLSID\\{clsid}\\InprocServer32");
        let h = create_key_at(root, &server)?;
        set_sz(h, null(), &dll)?;
        set_sz(h, w("ThreadingModel").as_ptr(), "Apartment")?;
        RegCloseKey(h);
        for category in &CATEGORIES {
            let cat = guid_braced(category);
            for path in [
                format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Category\\{cat}\\{clsid}"),
                format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Item\\{clsid}\\{cat}"),
            ] {
                let h = create_key_at(root, &path)?;
                RegCloseKey(h);
            }
        }
        let item = format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Item\\{clsid}");
        let h = create_key_at(root, &item)?;
        set_sz(h, w("Description").as_ptr(), DESCRIPTION)?;
        RegCloseKey(h);
    }
    Ok(())
}

fn register_profile_hkcu() -> Result<(), String> {
    let clsid = guid_braced(&ids::CLSID);
    let profile = guid_braced(&ids::PROFILE);
    let lang = format!("Software\\Microsoft\\CTF\\TIP\\{clsid}\\LanguageProfile\\0x{LANGID:08x}\\{profile}");
    unsafe {
        let h = create_key(&lang)?;
        set_sz(h, w("Description").as_ptr(), DESCRIPTION)?;
        set_dword(h, w("Enable").as_ptr(), 1)?;
        RegCloseKey(h);
        let item = format!("Software\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Item\\{clsid}");
        let h = create_key(&item)?;
        set_sz(h, w("Description").as_ptr(), DESCRIPTION)?;
        RegCloseKey(h);
        for category in &CATEGORIES {
            let cat = guid_braced(category);
            for path in [
                format!("Software\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Category\\{cat}\\{clsid}"),
                format!("Software\\Microsoft\\CTF\\TIP\\{clsid}\\Category\\Item\\{clsid}\\{cat}"),
            ] {
                let h = create_key(&path)?;
                RegCloseKey(h);
            }
        }
    }
    Ok(())
}

fn add_to_language_list() -> Result<bool, String> {
    let tip = ids::tip_id();
    let script = format!(
        "$ErrorActionPreference = 'Stop'; $tip = '{tip}'; $list = Get-WinUserLanguageList; $changed = $false; foreach ($l in $list) {{ if ($l.LanguageTag -eq 'zh-Hant-HK' -and -not ($l.InputMethodTips -contains $tip)) {{ [void]$l.InputMethodTips.Add($tip); $changed = $true }} }}; if ($changed) {{ Set-WinUserLanguageList -LanguageList $list -Force }}; if ($changed) {{ 'changed' }} else {{ 'same' }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // The switcher reads this value directly if the cmdlet refuses.
        let _ = set_tip_registry(&tip);
        return Err(err.trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.contains("changed"))
}

fn set_tip_registry(tip: &str) -> Result<(), String> {
    unsafe {
        let h = create_key("Control Panel\\International\\User Profile\\zh-Hant-HK")?;
        set_dword(h, w(tip).as_ptr(), 1)?;
        RegCloseKey(h);
    }
    Ok(())
}

unsafe fn create_key(path: &str) -> Result<HKEY, String> {
    let path = path.replacen("Software\\", "SOFTWARE\\", 1);
    create_key_at(HKEY_CURRENT_USER, &path)
}

unsafe fn create_key_at(root: HKEY, path: &str) -> Result<HKEY, String> {
    let mut h = null_mut();
    let name = w(path);
    let rc = RegCreateKeyExW(root, name.as_ptr(), 0, null(), 0, KEY_SET_VALUE | KEY_CREATE_SUB_KEY, null(), &mut h, null_mut());
    if rc != 0 {
        return Err(format!("RegCreateKeyEx {path} -> {rc}"));
    }
    Ok(h)
}

unsafe fn set_sz(key: HKEY, name: *const u16, value: &str) -> Result<(), String> {
    let v = w(value);
    let rc = RegSetValueExW(key, name, 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32);
    (rc == 0).then_some(()).ok_or_else(|| format!("RegSetValueEx {rc}"))
}

unsafe fn set_dword(key: HKEY, name: *const u16, value: u32) -> Result<(), String> {
    let rc = RegSetValueExW(key, name, 0, REG_DWORD, &value as *const u32 as *const u8, 4);
    (rc == 0).then_some(()).ok_or_else(|| format!("RegSetValueEx dword {rc}"))
}

unsafe fn release(unk: *mut core::ffi::c_void) {
    if unk.is_null() {
        return;
    }
    type Release = unsafe extern "system" fn(*mut core::ffi::c_void) -> u32;
    let vtbl = *(unk as *const *const Release);
    let release = *vtbl.add(2);
    release(unk);
}
