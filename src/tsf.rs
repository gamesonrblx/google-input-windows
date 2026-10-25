//! 粵拼 as a Windows text service. ctfmon loads this DLL into the foreground app
//! when Chinese (Jyutping) is the selected input method and delivers keys here.
//! Text is inserted through TSF, not SendInput, and the candidate box is the same
//! one the tray app draws.

use crate::active;
use crate::config::Config;
use crate::engine::{Engine, Key, Out};
use crate::gdi::{self, w};
use crate::google::{self, Cands, Query};
use crate::ids::{self, guid_eq, CLSID, DESCRIPTION};
use crate::popup::{self, HIT_NEXT, HIT_PREV};
use crate::tools;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write;
use std::mem::{offset_of, size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const S_OK: i32 = 0;
const S_FALSE: i32 = 1;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
const E_POINTER: i32 = 0x8000_4003_u32 as i32;
const E_FAIL: i32 = 0x8000_4005_u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057_u32 as i32;
const CLASS_E_CLASSNOTAVAILABLE: i32 = 0x8004_0111_u32 as i32;
const CLASS_E_NOAGGREGATION: i32 = 0x8004_0110_u32 as i32;
const IID_UNKNOWN: GUID = ids::guid(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_CLASS_FACTORY: GUID = ids::guid(0x0000_0001, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_TIP: GUID = ids::guid(0xAA80_E7F7, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
const IID_TIP_EX: GUID = ids::guid(0x6E4E_2102, 0xF9CD, 0x433D, [0xB4, 0x96, 0x30, 0x3C, 0xE0, 0x3A, 0x65, 0x07]);
const IID_KEY: GUID = ids::guid(0xAA80_E7F5, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
const IID_FOCUS: GUID = ids::guid(0xAA80_E80E, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
const IID_KEYSTROKE: GUID = ids::guid(0xAA80_E7F0, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
const IID_SOURCE: GUID = ids::guid(0x4EA4_8A35, 0x60AE, 0x446F, [0x8F, 0xD6, 0xE6, 0xA8, 0xD8, 0x24, 0x59, 0xF7]);
const IID_EDIT: GUID = ids::guid(0xAA80_E803, 0x2021, 0x11D2, [0x93, 0xE0, 0x00, 0x60, 0xB0, 0x67, 0xB8, 0x6E]);
const IID_INSERT: GUID = ids::guid(0x55CE_16BA, 0x3014, 0x41C1, [0x9C, 0xEB, 0xFA, 0xDE, 0x14, 0x46, 0xAC, 0x6C]);
const TF_ES_READWRITE_SYNC: u32 = 0x7; // TF_ES_SYNC | TF_ES_READWRITE
const TF_IAS_NOQUERY: u32 = 1;
const WM_CANDS: u32 = WM_APP + 2;
const WM_RENDER: u32 = WM_APP + 4;
const WM_CLICK: u32 = WM_APP + 5;
const TIMER_WAIT: usize = 1;

type Lookup = (Query, Option<Cands>);

thread_local! {
    static CURRENT: Cell<*mut Svc> = const { Cell::new(null_mut()) };
}

#[repr(C)]
struct Svc {
    tip_vtbl: *const TipVtbl,
    key_vtbl: *const KeyVtbl,
    focus_vtbl: *const FocusVtbl,
    refs: AtomicI32,
    thread_mgr: *mut c_void,
    client_id: u32,
    keys_advised: bool,
    sink_cookie: u32,
    mouse: HHOOK,
    hwnd: HWND,
    popup: HWND,
    eng: Engine,
    shift_tap: bool,
    cache: HashMap<Query, Cands>,
    pending: HashSet<Query>,
    swallowed: HashSet<u32>,
    waiting: Option<(Key, u32)>,
    queue: Vec<(u32, bool)>,
    last_down: u32,
    render_posted: bool,
    sending: bool,
}

#[repr(C)]
struct TipVtbl {
    qi: Qi,
    add: RefFn,
    release: RefFn,
    activate: unsafe extern "system" fn(*mut Svc, *mut c_void, u32) -> i32,
    deactivate: unsafe extern "system" fn(*mut Svc) -> i32,
    activate_ex: unsafe extern "system" fn(*mut Svc, *mut c_void, u32, u32) -> i32,
}
#[repr(C)]
struct KeyVtbl {
    qi: Qi,
    add: RefFn,
    release: RefFn,
    on_set_focus: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    on_test_down: KeyFn,
    on_down: KeyFn,
    on_test_up: KeyFn,
    on_up: KeyFn,
    on_preserved: unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, *mut i32) -> i32,
}
#[repr(C)]
struct FocusVtbl {
    qi: Qi,
    add: RefFn,
    release: RefFn,
    on_init: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    on_uninit: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    on_set_focus: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    on_push: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    on_pop: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
}
type Qi = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
type RefFn = unsafe extern "system" fn(*mut c_void) -> u32;
type KeyFn = unsafe extern "system" fn(*mut c_void, *mut c_void, usize, isize, *mut i32) -> i32;

static TIP_VTBL: TipVtbl = TipVtbl {
    qi: tip_qi,
    add: tip_add,
    release: tip_release,
    activate: tip_activate,
    deactivate: tip_deactivate,
    activate_ex: tip_activate_ex,
};
static KEY_VTBL: KeyVtbl = KeyVtbl {
    qi: key_qi,
    add: key_add,
    release: key_release,
    on_set_focus: key_set_focus,
    on_test_down: key_test_down,
    on_down: key_down,
    on_test_up: key_test_up,
    on_up: key_up,
    on_preserved: key_preserved,
};
static FOCUS_VTBL: FocusVtbl = FocusVtbl {
    qi: focus_qi,
    add: focus_add,
    release: focus_release,
    on_init: focus_ok,
    on_uninit: focus_ok,
    on_set_focus: focus_set,
    on_push: focus_ok,
    on_pop: focus_ok,
};

fn from_key(this: *mut c_void) -> *mut Svc {
    unsafe { (this as *mut u8).sub(offset_of!(Svc, key_vtbl)) as *mut Svc }
}
fn from_focus(this: *mut c_void) -> *mut Svc {
    unsafe { (this as *mut u8).sub(offset_of!(Svc, focus_vtbl)) as *mut Svc }
}

unsafe extern "system" fn tip_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    query(this as *mut Svc, riid, ppv)
}
unsafe extern "system" fn tip_add(this: *mut c_void) -> u32 {
    add_ref(this as *mut Svc)
}
unsafe extern "system" fn tip_release(this: *mut c_void) -> u32 {
    release_svc(this as *mut Svc)
}
unsafe extern "system" fn tip_activate(this: *mut Svc, tm: *mut c_void, tid: u32) -> i32 {
    (*this).activate(tm, tid)
}
unsafe extern "system" fn tip_activate_ex(this: *mut Svc, tm: *mut c_void, tid: u32, _flags: u32) -> i32 {
    (*this).activate(tm, tid)
}
unsafe extern "system" fn tip_deactivate(this: *mut Svc) -> i32 {
    (*this).deactivate();
    S_OK
}

unsafe extern "system" fn key_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    query(from_key(this), riid, ppv)
}
unsafe extern "system" fn key_add(this: *mut c_void) -> u32 {
    add_ref(from_key(this))
}
unsafe extern "system" fn key_release(this: *mut c_void) -> u32 {
    release_svc(from_key(this))
}
unsafe extern "system" fn key_set_focus(this: *mut c_void, foreground: i32) -> i32 {
    if foreground == 0 {
        let svc = &mut *from_key(this);
        svc.eng.reset();
        svc.eng.forget_context();
        svc.render();
    }
    S_OK
}
unsafe extern "system" fn key_test_down(this: *mut c_void, _: *mut c_void, vk: usize, _: isize, eaten: *mut i32) -> i32 {
    // Prediction only. Editing the document here is rejected by TSF, so the
    // real composition happens in OnKeyDown.
    eaten_from(eaten, (*from_key(this)).preview_down(vk as u32))
}
unsafe extern "system" fn key_down(this: *mut c_void, pic: *mut c_void, vk: usize, _: isize, eaten: *mut i32) -> i32 {
    eaten_from(eaten, (*from_key(this)).on_down(pic, vk as u32))
}
unsafe extern "system" fn key_test_up(this: *mut c_void, _: *mut c_void, vk: usize, _: isize, eaten: *mut i32) -> i32 {
    eaten_from(eaten, (*from_key(this)).preview_up(vk as u32))
}
unsafe extern "system" fn key_up(this: *mut c_void, pic: *mut c_void, vk: usize, _: isize, eaten: *mut i32) -> i32 {
    eaten_from(eaten, (*from_key(this)).on_up(pic, vk as u32))
}
unsafe extern "system" fn key_preserved(_: *mut c_void, _: *mut c_void, _: *const GUID, eaten: *mut i32) -> i32 {
    eaten_from(eaten, false)
}
fn eaten_from(out: *mut i32, eat: bool) -> i32 {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe { *out = i32::from(eat) };
    S_OK
}

unsafe extern "system" fn focus_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    query(from_focus(this), riid, ppv)
}
unsafe extern "system" fn focus_add(this: *mut c_void) -> u32 {
    add_ref(from_focus(this))
}
unsafe extern "system" fn focus_release(this: *mut c_void) -> u32 {
    release_svc(from_focus(this))
}
unsafe extern "system" fn focus_ok(_: *mut c_void, _: *mut c_void) -> i32 {
    S_OK
}
unsafe extern "system" fn focus_set(this: *mut c_void, _new: *mut c_void, _old: *mut c_void) -> i32 {
    let svc = &mut *from_focus(this);
    svc.eng.reset();
    svc.eng.forget_context();
    svc.render();
    S_OK
}

unsafe fn query(svc: *mut Svc, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    let iid = &*riid;
    let ptr = if guid_eq(iid, &IID_UNKNOWN) || guid_eq(iid, &IID_TIP) || guid_eq(iid, &IID_TIP_EX) {
        svc as *mut c_void
    } else if guid_eq(iid, &IID_KEY) {
        &mut (*svc).key_vtbl as *mut _ as *mut c_void
    } else if guid_eq(iid, &IID_FOCUS) {
        &mut (*svc).focus_vtbl as *mut _ as *mut c_void
    } else {
        *ppv = null_mut();
        return E_NOINTERFACE;
    };
    *ppv = ptr;
    add_ref(svc);
    S_OK
}

unsafe fn add_ref(svc: *mut Svc) -> u32 {
    (*svc).refs.fetch_add(1, Ordering::Relaxed) as u32 + 1
}
unsafe fn release_svc(svc: *mut Svc) -> u32 {
    let n = (*svc).refs.fetch_sub(1, Ordering::Release) - 1;
    if n == 0 {
        drop(Box::from_raw(svc));
    }
    n as u32
}
unsafe fn release_unknown(unk: *mut c_void) {
    if !unk.is_null() {
        let vtbl = *(unk as *const *const RefFn);
        (*vtbl.add(2))(unk);
    }
}
unsafe fn qi_unknown(unk: *mut c_void, iid: &GUID, out: *mut *mut c_void) -> i32 {
    let vtbl = *(unk as *const *const Qi);
    (*vtbl)(unk, iid, out)
}

impl Svc {
    fn new() -> Box<Svc> {
        let cfg = Config::load();
        let tool = tools::find(ids::JYUTPING).expect("粵拼 is in the tool table");
        Box::new(Svc {
            tip_vtbl: &TIP_VTBL,
            key_vtbl: &KEY_VTBL,
            focus_vtbl: &FOCUS_VTBL,
            refs: AtomicI32::new(1),
            thread_mgr: null_mut(),
            client_id: 0,
            keys_advised: false,
            sink_cookie: 0,
            mouse: null_mut(),
            hwnd: null_mut(),
            popup: null_mut(),
            eng: Engine::new(tool, cfg.state),
            shift_tap: cfg.shift_tap,
            cache: HashMap::new(),
            pending: HashSet::new(),
            swallowed: HashSet::new(),
            waiting: None,
            queue: Vec::new(),
            last_down: 0,
            render_posted: false,
            sending: false,
        })
    }

    fn activate(&mut self, tm: *mut c_void, tid: u32) -> i32 {
        if tm.is_null() {
            return E_INVALIDARG;
        }
        // Mark this thread so the tray hook types Jyutping. The hook runs first
        // and inserts the characters; a key sink here only swallowed them.
        self.client_id = tid;
        self.thread_mgr = tm;
        unsafe { add_unknown(tm) };
        active::set_here(true);
        trace("activate");
        S_OK
    }

    fn deactivate(&mut self) {
        active::set_here(false);
        self.unadvise();
        if !self.mouse.is_null() {
            unsafe { UnhookWindowsHookEx(self.mouse) };
            self.mouse = null_mut();
        }
        CURRENT.with(|c| {
            if c.get() == self as *mut Svc {
                c.set(null_mut());
            }
        });
        if !self.popup.is_null() {
            popup::hide(self.popup);
            unsafe { DestroyWindow(self.popup) };
            self.popup = null_mut();
        }
        if !self.hwnd.is_null() {
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = null_mut();
        }
        if !self.thread_mgr.is_null() {
            unsafe { release_unknown(self.thread_mgr) };
            self.thread_mgr = null_mut();
        }
        self.client_id = 0;
        self.eng.reset();
    }

    fn make_window(&self, hinst: HINSTANCE) -> HWND {
        unsafe {
            let cls = w("GoogleInputJyutpingMsg");
            let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinst, lpszClassName: cls.as_ptr(), ..zeroed() };
            RegisterClassW(&wc);
            CreateWindowExW(0, cls.as_ptr(), w(DESCRIPTION).as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), hinst, null())
        }
    }

    fn advise_keys(&mut self) -> i32 {
        let mut mgr = null_mut();
        let hr = unsafe { qi_unknown(self.thread_mgr, &IID_KEYSTROKE, &mut mgr) };
        if hr < 0 {
            return hr;
        }
        type Advise = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, i32) -> i32;
        let hr = unsafe {
            let vtbl = *(mgr as *const *const Advise);
            let sink = &mut self.key_vtbl as *mut _ as *mut c_void;
            (*vtbl.add(3))(mgr, self.client_id, sink, 1)
        };
        unsafe { release_unknown(mgr) };
        self.keys_advised = hr >= 0;
        hr
    }

    fn advise_focus(&mut self) -> i32 {
        let mut src = null_mut();
        let hr = unsafe { qi_unknown(self.thread_mgr, &IID_SOURCE, &mut src) };
        if hr < 0 {
            return hr;
        }
        type Advise = unsafe extern "system" fn(*mut c_void, *const GUID, *mut c_void, *mut u32) -> i32;
        let mut cookie = 0u32;
        let hr = unsafe {
            let vtbl = *(src as *const *const Advise);
            let sink = &mut self.focus_vtbl as *mut _ as *mut c_void;
            (*vtbl.add(3))(src, &IID_FOCUS, sink, &mut cookie)
        };
        unsafe { release_unknown(src) };
        if hr >= 0 {
            self.sink_cookie = cookie;
        }
        hr
    }

    fn unadvise(&mut self) {
        if self.thread_mgr.is_null() {
            return;
        }
        if self.keys_advised {
            let mut mgr = null_mut();
            if unsafe { qi_unknown(self.thread_mgr, &IID_KEYSTROKE, &mut mgr) } >= 0 {
                type Unadvise = unsafe extern "system" fn(*mut c_void, u32) -> i32;
                unsafe {
                    let vtbl = *(mgr as *const *const Unadvise);
                    (*vtbl.add(4))(mgr, self.client_id);
                    release_unknown(mgr);
                }
            }
            self.keys_advised = false;
        }
        if self.sink_cookie != 0 {
            let mut src = null_mut();
            if unsafe { qi_unknown(self.thread_mgr, &IID_SOURCE, &mut src) } >= 0 {
                type Unadvise = unsafe extern "system" fn(*mut c_void, u32) -> i32;
                unsafe {
                    let vtbl = *(src as *const *const Unadvise);
                    (*vtbl.add(4))(src, self.sink_cookie);
                    release_unknown(src);
                }
            }
            self.sink_cookie = 0;
        }
    }

    fn preview_down(&self, vk: u32) -> bool {
        if self.sending {
            return false;
        }
        if self.waiting.is_some() {
            return true;
        }
        let m = mods();
        if self.eng.tool.chinese && ((vk == VK_SPACE as u32 && m == crate::config::SHIFT) || (vk == VK_OEM_PERIOD as u32 && m == crate::config::CTRL)) {
            return true;
        }
        if m & (crate::config::CTRL | crate::config::ALT) != 0 || held(VK_LWIN) || held(VK_RWIN) {
            return false;
        }
        if m == crate::config::SHIFT && self.eng.composing() && matches!(vk as VIRTUAL_KEY, VK_BACK | VK_TAB | VK_DELETE | VK_PRIOR..=VK_DOWN) {
            return true;
        }
        let Some(k) = to_key(vk) else { return false };
        let mut eng = self.eng.clone();
        let out = eng.key(k);
        out.swallow || out.wait
    }

    fn preview_up(&self, vk: u32) -> bool {
        !self.sending && (self.waiting.is_some() || self.swallowed.contains(&vk))
    }

    fn on_down(&mut self, ctx: *mut c_void, vk: u32) -> bool {
        if self.sending {
            return false;
        }
        if self.waiting.is_some() {
            self.queue.push((vk, true));
            return true;
        }
        self.last_down = vk;
        let m = mods();
        if self.eng.tool.chinese && vk == VK_SPACE as u32 && m == crate::config::SHIFT {
            let s = self.eng.toggle_width();
            self.persist_state();
            self.insert(ctx, &s);
            self.render();
            return self.swallow(vk);
        }
        if self.eng.tool.chinese && vk == VK_OEM_PERIOD as u32 && m == crate::config::CTRL {
            self.eng.toggle_punct();
            self.persist_state();
            self.render();
            return self.swallow(vk);
        }
        if m & (crate::config::CTRL | crate::config::ALT) != 0 || held(VK_LWIN) || held(VK_RWIN) {
            if !is_modifier(vk) {
                self.eng.reset();
                self.eng.forget_context();
                self.render();
            }
            return false;
        }
        if m == crate::config::SHIFT && self.eng.composing() && matches!(vk as VIRTUAL_KEY, VK_BACK | VK_TAB | VK_DELETE | VK_PRIOR..=VK_DOWN) {
            return self.swallow(vk);
        }
        let Some(k) = to_key(vk) else { return false };
        self.feed(ctx, k, vk)
    }

    fn on_up(&mut self, ctx: *mut c_void, vk: u32) -> bool {
        if self.sending {
            return false;
        }
        if self.waiting.is_some() {
            self.queue.push((vk, false));
            return true;
        }
        let shift = vk == VK_LSHIFT as u32 || vk == VK_RSHIFT as u32;
        if shift && self.last_down == vk && self.shift_tap && self.eng.tool.chinese {
            let s = self.eng.toggle_lang();
            self.persist_state();
            self.insert(ctx, &s);
            self.render();
        }
        self.swallowed.remove(&vk)
    }

    fn feed(&mut self, ctx: *mut c_void, k: Key, vk: u32) -> bool {
        let out = self.eng.key(k);
        if out.wait {
            self.waiting = Some((k, vk));
            unsafe { SetTimer(self.hwnd, TIMER_WAIT, 2000, None) };
            self.after();
            return self.swallow(vk);
        }
        self.apply_out(ctx, out, vk)
    }

    fn apply_out(&mut self, ctx: *mut c_void, out: Out, vk: u32) -> bool {
        if !out.commit.is_empty() {
            self.insert(ctx, &out.commit);
        }
        self.after();
        self.draw();
        out.swallow && self.swallow(vk)
    }

    fn swallow(&mut self, vk: u32) -> bool {
        if vk != 0 {
            self.swallowed.insert(vk);
        }
        true
    }

    fn after(&mut self) {
        if let Some(q) = self.eng.query() {
            if let Some(c) = self.cache.get(&q).cloned() {
                self.eng.answer(&q, c);
            } else if self.pending.insert(q.clone()) {
                let hwnd = self.hwnd as usize;
                std::thread::spawn(move || {
                    let r = google::fetch(&q, Duration::from_secs(5));
                    let msg: Box<Lookup> = Box::new((q, r));
                    unsafe { PostMessageW(hwnd as HWND, WM_CANDS, 0, Box::into_raw(msg) as LPARAM) };
                });
            }
        }
        self.render();
    }

    fn got(&mut self, ctx: *mut c_void, q: Query, r: Option<Cands>) {
        self.pending.remove(&q);
        match r {
            Some(c) => {
                if self.cache.len() > 2000 {
                    self.cache.clear();
                }
                self.cache.insert(q.clone(), c.clone());
                self.eng.answer(&q, c);
            }
            None => self.eng.failed(&q),
        }
        if self.waiting.is_some() && self.eng.query().is_none() {
            self.finish_wait(ctx);
        } else {
            self.after();
        }
    }

    fn finish_wait(&mut self, ctx: *mut c_void) {
        let Some((k, vk)) = self.waiting.take() else { return };
        unsafe { KillTimer(self.hwnd, TIMER_WAIT) };
        if let Some(q) = self.eng.query() {
            self.eng.failed(&q);
        }
        let out = self.eng.resume(k);
        self.apply_out(ctx, out, vk);
        for (vk, down) in std::mem::take(&mut self.queue) {
            if self.waiting.is_some() {
                self.queue.push((vk, down));
            } else if down {
                let _ = self.on_down(ctx, vk);
            } else {
                let _ = self.on_up(ctx, vk);
            }
        }
    }

    fn render(&mut self) {
        if !self.render_posted && !self.hwnd.is_null() {
            self.render_posted = true;
            unsafe { PostMessageW(self.hwnd, WM_RENDER, 0, 0) };
        }
    }

    fn draw(&mut self) {
        self.render_posted = false;
        if self.popup.is_null() {
            return;
        }
        if !self.eng.composing() {
            popup::hide(self.popup);
            return;
        }
        let t = self.eng.tool;
        popup::show(self.popup, self.eng.view(), tools::font_for(t), t.vertical, gdi::caret_pos());
    }

    fn popup_hit(&mut self, ctx: *mut c_void, hit: usize) {
        if hit == HIT_PREV || hit == HIT_NEXT {
            self.eng.click_page(hit == HIT_NEXT);
            self.after();
            return;
        }
        let out = self.eng.click(hit);
        self.apply_out(ctx, out, 0);
    }

    fn insert(&mut self, ctx: *mut c_void, text: &str) {
        if text.is_empty() {
            return;
        }
        if !ctx.is_null() && self.client_id != 0 && unsafe { insert_text(ctx, self.client_id, text) } {
            return;
        }
        // TSF refused the edit (some windows do, during the wrong callback).
        // Type it the same way the tray app does, and don't reprocess those keys.
        self.sending = true;
        send_unicode(text);
        self.sending = false;
    }

    fn persist_state(&self) {
        let mut cfg = Config::load();
        if cfg.state != self.eng.state {
            cfg.state = self.eng.state;
            cfg.save();
        }
    }

    fn focus_context(&self) -> *mut c_void {
        unsafe {
            if self.thread_mgr.is_null() {
                return null_mut();
            }
            type Get = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
            let vtbl = *(self.thread_mgr as *const *const Get);
            let mut doc = null_mut();
            if (*vtbl.add(7))(self.thread_mgr, &mut doc) < 0 || doc.is_null() {
                return null_mut();
            }
            let dv = *(doc as *const *const Get);
            let mut ctx = null_mut();
            let hr = (*dv.add(6))(doc, &mut ctx);
            release_unknown(doc);
            if hr < 0 { null_mut() } else { ctx }
        }
    }
}

unsafe fn add_unknown(unk: *mut c_void) {
    let vtbl = *(unk as *const *const RefFn);
    (*vtbl.add(1))(unk);
}

fn held(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}
fn mods() -> u8 {
    (if held(VK_SHIFT) { crate::config::SHIFT } else { 0 })
        | (if held(VK_CONTROL) { crate::config::CTRL } else { 0 })
        | (if held(VK_MENU) { crate::config::ALT } else { 0 })
}
fn is_modifier(vk: u32) -> bool {
    [VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_MENU, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN, VK_CAPITAL]
        .contains(&(vk as VIRTUAL_KEY))
}
fn typed_char(vk: u32) -> Option<char> {
    // Jyutping is typed on the physical QWERTY keys, whichever layout is current.
    if (0x41..=0x5A).contains(&vk) {
        let mut ch = (b'a' + (vk as u8 - 0x41)) as char;
        let caps = unsafe { GetKeyState(VK_CAPITAL as i32) & 1 != 0 };
        if held(VK_SHIFT) ^ caps {
            ch = ch.to_ascii_uppercase();
        }
        return Some(ch);
    }
    if (0x30..=0x39).contains(&vk) && !held(VK_SHIFT) {
        return Some(char::from_u32(vk).unwrap());
    }
    unsafe {
        let mut state = [0u8; 256];
        if held(VK_SHIFT) {
            state[VK_SHIFT as usize] = 0x80;
        }
        state[VK_CAPITAL as usize] = (GetKeyState(VK_CAPITAL as i32) & 1) as u8;
        let layout = GetKeyboardLayout(0);
        let mut buf = [0u16; 4];
        let scan = MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, layout);
        let n = ToUnicodeEx(vk, scan, state.as_ptr(), buf.as_mut_ptr(), 4, 4, layout);
        (n == 1 && buf[0] >= 0x20).then(|| char::from_u32(buf[0] as u32)).flatten()
    }
}
fn to_key(vk: u32) -> Option<Key> {
    Some(match vk as VIRTUAL_KEY {
        VK_SPACE => Key::Space,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Esc,
        VK_BACK => Key::Back,
        VK_DELETE => Key::Delete,
        VK_TAB => Key::Tab,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_PRIOR => Key::PgUp,
        VK_NEXT => Key::PgDn,
        _ if is_modifier(vk) => return None,
        _ => Key::Char(typed_char(vk)?),
    })
}

unsafe fn insert_text(ctx: *mut c_void, client_id: u32, text: &str) -> bool {
    let edit = Box::into_raw(Box::new(Edit {
        lp_vtbl: &EDIT_VTBL,
        refs: AtomicI32::new(1),
        text: text.encode_utf16().collect(),
    }));
    EDIT_CTX.with(|c| c.set(ctx));
    type Request = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32, *mut i32) -> i32;
    let vtbl = *(ctx as *const *const Request);
    let mut hr_session = 0i32;
    let hr = (*vtbl.add(3))(ctx, client_id, edit as *mut c_void, TF_ES_READWRITE_SYNC, &mut hr_session);
    EDIT_CTX.with(|c| c.set(null_mut()));
    edit_release(edit as *mut c_void);
    hr >= 0 && hr_session >= 0
}

fn send_unicode(s: &str) {
    let inputs: Vec<INPUT> = s
        .encode_utf16()
        .flat_map(|u| {
            [
                INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: u, dwFlags: KEYEVENTF_UNICODE, time: 0, dwExtraInfo: 0 } } },
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: u, dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, time: 0, dwExtraInfo: 0 } },
                },
            ]
        })
        .collect();
    if !inputs.is_empty() {
        unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    }
}

#[repr(C)]
struct Edit {
    lp_vtbl: *const EditVtbl,
    refs: AtomicI32,
    text: Vec<u16>,
}
#[repr(C)]
struct EditVtbl {
    qi: Qi,
    add: RefFn,
    release: RefFn,
    do_edit: unsafe extern "system" fn(*mut Edit, u32) -> i32,
}
static EDIT_VTBL: EditVtbl = EditVtbl { qi: edit_qi, add: edit_add, release: edit_release, do_edit: edit_do };

unsafe extern "system" fn edit_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    let iid = &*riid;
    if guid_eq(iid, &IID_UNKNOWN) || guid_eq(iid, &IID_EDIT) {
        *ppv = this;
        edit_add(this);
        S_OK
    } else {
        *ppv = null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn edit_add(this: *mut c_void) -> u32 {
    (*(this as *mut Edit)).refs.fetch_add(1, Ordering::Relaxed) as u32 + 1
}
unsafe extern "system" fn edit_release(this: *mut c_void) -> u32 {
    let edit = this as *mut Edit;
    let n = (*edit).refs.fetch_sub(1, Ordering::Release) - 1;
    if n == 0 {
        drop(Box::from_raw(edit));
    }
    n as u32
}
unsafe extern "system" fn edit_do(this: *mut Edit, ec: u32) -> i32 {
    // The context is the object RequestEditSession was called on. It is current
    // only for this synchronous call, passed through the thread-local below.
    let ctx = EDIT_CTX.with(|c| c.get());
    if ctx.is_null() {
        return E_FAIL;
    }
    let mut insert = null_mut();
    if qi_unknown(ctx, &IID_INSERT, &mut insert) < 0 {
        return E_FAIL;
    }
    type InsertFn = unsafe extern "system" fn(*mut c_void, u32, u32, *const u16, i32, *mut *mut c_void) -> i32;
    let vtbl = *(insert as *const *const InsertFn);
    let text = &(*this).text;
    let hr = (*vtbl.add(3))(insert, ec, TF_IAS_NOQUERY, text.as_ptr(), text.len() as i32, null_mut());
    release_unknown(insert);
    hr
}

thread_local! {
    static EDIT_CTX: Cell<*mut c_void> = const { Cell::new(null_mut()) };
}

// insert_text has to publish the context for the synchronous DoEditSession.
// Rewrite the call site by wrapping it; the function above is replaced below.
#[allow(dead_code)]
fn _edit_ctx_slot() {}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let svc = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Svc;
    if svc.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let svc = &mut *svc;
    match msg {
        WM_CANDS => {
            let boxed = Box::from_raw(lp as *mut Lookup);
            let ctx = svc.focus_context();
            with_edit_ctx(ctx, || {
                let (q, r) = *boxed;
                svc.got(ctx, q, r);
            });
            release_unknown(ctx);
            0
        }
        WM_RENDER => {
            svc.draw();
            0
        }
        WM_TIMER if wp == TIMER_WAIT => {
            let ctx = svc.focus_context();
            with_edit_ctx(ctx, || svc.finish_wait(ctx));
            release_unknown(ctx);
            0
        }
        popup::WM_POPUP_HIT => {
            let ctx = svc.focus_context();
            with_edit_ctx(ctx, || svc.popup_hit(ctx, wp));
            release_unknown(ctx);
            0
        }
        WM_CLICK => {
            svc.eng.reset();
            svc.eng.forget_context();
            svc.draw();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn with_edit_ctx(ctx: *mut c_void, f: impl FnOnce()) {
    EDIT_CTX.with(|c| c.set(ctx));
    f();
    EDIT_CTX.with(|c| c.set(null_mut()));
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wp as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
        let (hwnd, popup) = CURRENT.with(|c| {
            let svc = c.get();
            if svc.is_null() { (null_mut(), null_mut()) } else { ((*svc).hwnd, (*svc).popup) }
        });
        if !hwnd.is_null() {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            if WindowFromPoint(pt) != popup {
                PostMessageW(hwnd, WM_CLICK, 0, 0);
            }
        }
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}

fn trace(msg: &str) {
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        let dir = std::path::PathBuf::from(dir).join("GoogleInputTools");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join("tsf.log")) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn dll_instance() -> HINSTANCE {
    unsafe {
        let mut h = null_mut();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            DllGetClassObject as *const () as usize as *const u16,
            &mut h,
        );
        h
    }
}

// --- class factory -------------------------------------------------------

#[repr(C)]
struct Factory {
    lp_vtbl: *const FactoryVtbl,
    refs: AtomicI32,
}
#[repr(C)]
struct FactoryVtbl {
    qi: Qi,
    add: RefFn,
    release: RefFn,
    create: unsafe extern "system" fn(*mut Factory, *mut c_void, *const GUID, *mut *mut c_void) -> i32,
    lock: unsafe extern "system" fn(*mut Factory, i32) -> i32,
}
static FACTORY_VTBL: FactoryVtbl = FactoryVtbl { qi: factory_qi, add: factory_add, release: factory_release, create: factory_create, lock: factory_lock };

fn factory() -> *mut Factory {
    use std::sync::atomic::AtomicPtr;
    static F: AtomicPtr<Factory> = AtomicPtr::new(null_mut());
    let existing = F.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let created = Box::into_raw(Box::new(Factory { lp_vtbl: &FACTORY_VTBL, refs: AtomicI32::new(1) }));
    match F.compare_exchange(null_mut(), created, Ordering::Release, Ordering::Acquire) {
        Ok(_) => created,
        Err(winner) => {
            drop(unsafe { Box::from_raw(created) });
            winner
        }
    }
}

unsafe extern "system" fn factory_qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    let iid = &*riid;
    if guid_eq(iid, &IID_UNKNOWN) || guid_eq(iid, &IID_CLASS_FACTORY) {
        *ppv = this;
        factory_add(this);
        S_OK
    } else {
        *ppv = null_mut();
        E_NOINTERFACE
    }
}
unsafe extern "system" fn factory_add(this: *mut c_void) -> u32 {
    (*(this as *mut Factory)).refs.fetch_add(1, Ordering::Relaxed) as u32 + 1
}
unsafe extern "system" fn factory_release(this: *mut c_void) -> u32 {
    let n = (*(this as *mut Factory)).refs.fetch_sub(1, Ordering::Relaxed) - 1;
    n.max(1) as u32 // static factory, never freed
}
unsafe extern "system" fn factory_lock(_: *mut Factory, _: i32) -> i32 {
    S_OK
}
unsafe extern "system" fn factory_create(_: *mut Factory, outer: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    if ppv.is_null() {
        return E_POINTER;
    }
    let svc = Box::into_raw(Svc::new());
    let hr = query(svc, riid, ppv);
    release_svc(svc); // QI's reference is the one handed out; drop the creation ref
    if hr < 0 {
        *ppv = null_mut();
    }
    hr
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> i32 {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    if !guid_eq(&*rclsid, &CLSID) {
        *ppv = null_mut();
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    trace("class object");
    factory_qi(factory() as *mut c_void, riid, ppv)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> i32 {
    S_FALSE
}
