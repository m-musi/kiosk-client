//! Windows 平台锁定实现
//!
//! 1. 低级键盘钩子（WH_KEYBOARD_LL）：拦截 Alt+Tab / Win / Ctrl+Shift+Esc / Alt+F4 等
//! 2. 焦点守护线程：500ms 巡检，焦点被夺走立即夺回 + 保持 TOPMOST
//! 3. 触控板三指/四指手势：进入考试时禁用，退出考试时恢复
//! 4. 进程监控线程：sysinfo 每 2s 扫描黑名单

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::WebviewWindow;

use super::{
    name_matches, COMMUNICATION_APPS, AUTO_KILL_COMMUNICATION, AUTO_KILL_REMOTE,
    REMOTE_CONTROL_APPS,
};

static HOOK_STARTED: AtomicBool = AtomicBool::new(false);

// ---------------- 日志（release 无控制台，落盘到临时目录） ----------------

fn log_file() -> std::path::PathBuf {
    std::env::temp_dir().join("elecbalance-kiosk.log")
}

fn log(msg: &str) {
    let line = format!("[{}] {}\n", chrono_like_now(), msg);
    eprint!("{}", line);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file())
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 简易时间戳（避免引入 chrono）
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let years = 1970 + (days / 365);
    format!("{year:04}-{doy:03} {h:02}:{m:02}:{s:02}", year = years, doy = days % 365 + 1)
}

// ---------------- 入口 ----------------

pub fn activate(window: &WebviewWindow) {
    let _ = window.set_fullscreen(true);
    let _ = window.set_decorations(false);
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    super::LOCK_ACTIVE.store(true, Ordering::SeqCst);
    precheck_and_log();
    gestures::disable();

    let hwnd = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    if hwnd != 0 {
        std::thread::spawn(move || focus_guardian(hwnd));
    }
    if !HOOK_STARTED.swap(true, Ordering::SeqCst) {
        std::thread::spawn(keyboard_hook_thread);
    }
    std::thread::spawn(process_watcher);

    log("kiosk activated");
}

/// 退出考试：关闭所有锁定，恢复用户触控板手势设置
pub fn deactivate(_window: &WebviewWindow) {
    super::LOCK_ACTIVE.store(false, Ordering::SeqCst);
    gestures::restore();
    log("kiosk deactivated, returned to guide");
}

// ---------------- 启动预检 ----------------

pub fn precheck_and_log() {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_processes();

    let mut found_remote = Vec::new();
    let mut found_comm = Vec::new();

    for (_pid, process) in sys.processes() {
        let name = process.name().to_lowercase();
        if name_matches(&name, REMOTE_CONTROL_APPS) {
            found_remote.push(name.clone());
        } else if name_matches(&name, COMMUNICATION_APPS) {
            found_comm.push(name.clone());
        }
    }

    if !found_remote.is_empty() {
        log(&format!("precheck: 检测到远控软件 {:?}", found_remote));
        if AUTO_KILL_REMOTE {
            kill_matched(&sys, REMOTE_CONTROL_APPS);
        }
    }
    if !found_comm.is_empty() {
        log(&format!("precheck: 检测到通讯软件 {:?}", found_comm));
    }
}

// ---------------- 焦点守护 + TOPMOST ----------------

fn focus_guardian(hwnd: isize) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, VK_MENU, KEYEVENTF_KEYUP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SetForegroundWindow,
        SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_SHOWMAXIMIZED,
    };

    let target = hwnd as isize;
    loop {
        thread::sleep(Duration::from_millis(500));
        if !super::LOCK_ACTIVE.load(Ordering::SeqCst) {
            break;
        }
        unsafe {
            let target_h: HWND = target as *mut core::ffi::c_void;
            if IsIconic(target_h) != 0 {
                ShowWindow(target_h, SW_SHOWMAXIMIZED);
            }
            let fg = GetForegroundWindow();
            let mut fg_pid: u32 = 0;
            if !fg.is_null() {
                GetWindowThreadProcessId(fg, &mut fg_pid);
            }
            let fg_is_ours = fg_pid != 0 && fg_pid == GetCurrentProcessId();

            if !fg_is_ours && fg != target_h {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                let _ = SetForegroundWindow(target_h);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            }
            if !fg_is_ours {
                SetWindowPos(
                    target_h,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                );
                // 重新把悬浮退出按钮置顶，避免被重新置顶的考试窗口盖住
                let title: Vec<u16> = "退出考试".encode_utf16().chain(std::iter::once(0)).collect();
                let fab = FindWindowW(std::ptr::null(), title.as_ptr());
                if !fab.is_null() {
                    SetWindowPos(
                        fab,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                    );
                }
            }
        }
    }
}

// ---------------- 低级键盘钩子 ----------------

fn keyboard_hook_thread() {
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_F4, VK_F11, VK_LWIN, VK_RWIN, VK_SHIFT,
        VK_SPACE, VK_TAB,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
    };

    const LLKHF_ALTDOWN: u32 = 0x20;
    const WM_KEYDOWN: usize = 0x0100;
    const WM_SYSKEYDOWN: usize = 0x0104;

    unsafe extern "system" fn ll_keyboard_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // 引导页阶段不拦截任何按键
        if code >= 0
            && (wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN)
            && super::LOCK_ACTIVE.load(Ordering::SeqCst)
        {
            let info: &KBDLLHOOKSTRUCT = &*(lparam as *const KBDLLHOOKSTRUCT);
            let vk = info.vkCode;
            let alt = (info.flags & LLKHF_ALTDOWN) != 0;
            let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
            let shift = (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0;

            let blocked = alt && vk == VK_TAB as u32           // Alt+Tab
                || alt && vk == VK_ESCAPE as u32               // Alt+Esc
                || alt && vk == VK_F4 as u32                   // Alt+F4
                || alt && vk == VK_SPACE as u32                // Alt+Space 系统菜单
                || vk == VK_LWIN as u32                        // Win 键（含 Win+Tab 切任务视图）
                || vk == VK_RWIN as u32
                || ctrl && shift && vk == VK_ESCAPE as u32     // Ctrl+Shift+Esc 任务管理器
                || ctrl && vk == VK_ESCAPE as u32              // Ctrl+Esc 开始菜单
                || vk == VK_F11 as u32;                        // F11 切全屏

            if blocked {
                log(&format!("blocked hotkey vkCode=0x{vk:x}"));
                return 1;
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }

    unsafe {
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL as i32,
            Some(ll_keyboard_proc),
            std::ptr::null_mut(),
            0,
        );
        if hook.is_null() {
            log("ERROR: SetWindowsHookExW failed");
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(hook);
    }
}

// ---------------- 触控板三指/四指手势 ----------------

mod gestures {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use windows_sys::core::PCWSTR;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, REG_DWORD,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    const KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\PrecisionTouchPad";
    const VALUES: [&str; 2] = ["ThreeFingerSlideEnabled", "FourFingerSlideEnabled"];

    /// 保存进入考试前的原始值，退出考试时恢复；None 表示原本不存在
    static SAVED: Mutex<Option<HashMap<String, Option<u32>>>> = Mutex::new(None);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn open_key() -> Option<HKEY> {
        let path = wide(KEY_PATH);
        let mut key: HKEY = std::ptr::null_mut();
        let code = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
        if code == 0 { Some(key) } else { None }
    }

    fn read_dword(key: HKEY, name: &str) -> Option<u32> {
        let n = wide(name);
        let mut data = [0u8; 4];
        let mut len = 4u32;
        let code = unsafe {
            RegQueryValueExW(
                key,
                n.as_ptr() as PCWSTR,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                data.as_mut_ptr(),
                &mut len,
            )
        };
        if code == 0 && len == 4 {
            Some(u32::from_ne_bytes(data))
        } else {
            None
        }
    }

    fn write_dword(key: HKEY, name: PCWSTR, value: u32) {
        unsafe {
            RegSetValueExW(
                key,
                name,
                0,
                REG_DWORD,
                &value as *const u32 as *const u8,
                4,
            );
        }
    }

    fn broadcast() {
        let section = wide("PrecisionTouchPad");
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                section.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                1000,
                std::ptr::null_mut(),
            );
        }
    }

    pub fn disable() {
        let Some(key) = open_key() else { return };
        let mut saved = HashMap::new();
        for name in VALUES {
            let n = wide(name);
            saved.insert(name.to_string(), read_dword(key, name));
            write_dword(key, n.as_ptr() as PCWSTR, 0);
        }
        unsafe { RegCloseKey(key) };
        let mut guard = SAVED.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(saved);
        drop(guard);
        broadcast();
    }

    pub fn restore() {
        let saved = SAVED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(saved) = saved else { return };
        let Some(key) = open_key() else { return };
        for (name, old) in saved {
            let n = wide(&name);
            match old {
                Some(value) => write_dword(key, n.as_ptr() as PCWSTR, value),
                None => {
                    unsafe { RegDeleteValueW(key, n.as_ptr() as PCWSTR) };
                }
            }
        }
        unsafe { RegCloseKey(key) };
        broadcast();
    }
}

// ---------------- 进程监控 ----------------

static REPORTED: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

fn kill_matched(sys: &sysinfo::System, patterns: &[&str]) {
    for (pid, process) in sys.processes() {
        let name = process.name().to_lowercase();
        if name_matches(&name, patterns) && pid.as_u32() != std::process::id() {
            log(&format!("killing blocked process: {} (pid {})", name, pid));
            let _ = process.kill();
        }
    }
}

fn process_watcher() {
    let mut sys = sysinfo::System::new_all();
    loop {
        thread::sleep(Duration::from_secs(2));
        if !super::LOCK_ACTIVE.load(Ordering::SeqCst) {
            break;
        }
        sys.refresh_processes();

        for (pid, process) in sys.processes() {
            if pid.as_u32() == std::process::id() {
                continue;
            }
            let name = process.name().to_lowercase();

            let category = if name_matches(&name, REMOTE_CONTROL_APPS) {
                Some(("remote", REMOTE_CONTROL_APPS, AUTO_KILL_REMOTE))
            } else if name_matches(&name, COMMUNICATION_APPS) {
                Some(("communication", COMMUNICATION_APPS, AUTO_KILL_COMMUNICATION))
            } else {
                None
            };

            if let Some((cat, _patterns, auto_kill)) = category {
                let should_report = REPORTED
                    .lock()
                    .map(|mut set| set.insert(name.clone()))
                    .unwrap_or(false);

                if should_report {
                    log(&format!("DETECTED {cat} process: {name} (pid {pid})"));
                }
                if auto_kill {
                    log(&format!("killing {cat} process: {name} (pid {pid})"));
                    let _ = process.kill();
                }
            }
        }
    }
}
