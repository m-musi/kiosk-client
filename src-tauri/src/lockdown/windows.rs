//! Windows 平台锁定实现
//!
//! 1. 低级键盘钩子（WH_KEYBOARD_LL）：拦截 Alt+Tab / Win / Ctrl+Shift+Esc / Alt+F4 等
//! 2. 焦点守护线程：500ms 巡检，焦点被夺走立即夺回 + 保持 TOPMOST
//! 3. 进程监控线程：sysinfo 每 2s 扫描黑名单（事件级方案见技术路线阶段二）

use std::collections::HashSet;
use std::io::Write;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::WebviewWindow;

use super::{
    name_matches, COMMUNICATION_APPS, AUTO_KILL_COMMUNICATION, AUTO_KILL_REMOTE,
    REMOTE_CONTROL_APPS,
};

// ---------------- 日志（release 无控制台，落盘到临时目录） ----------------

fn log_file() -> std::path::PathBuf {
    std::env::temp_dir().join("elecbalance-kiosk.log")
}

fn log(msg: &str) {
    let line = format!(
        "[{}] {}\n",
        chrono_like_now(),
        msg
    );
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
    // 仅用于日志排序，粗略转成本地可读格式
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 从 2026-01-01 起 rough 计算（仅日志用途，误差不影响排查）
    let years = 1970 + (days / 365);
    format!("{year:04}-{doy:03} {h:02}:{m:02}:{s:02}", year = years, doy = days % 365 + 1)
}

// ---------------- 入口 ----------------

pub fn activate(window: &WebviewWindow) {
    // 1. 全屏 + 置顶 + 无边框（配置里已是 decorations:false）
    let _ = window.set_fullscreen(true);
    let _ = window.set_decorations(false);
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    // 2. 启动时进程预检
    precheck_and_log();

    // 3. 取原生 HWND，启动守护线程
    #[cfg(windows)]
    {
        let hwnd = match window.hwnd() {
            Ok(h) => h.0 as isize,
            Err(_) => 0,
        };
        if hwnd != 0 {
            // 焦点守护（主进程内直接启动，简单可靠）
            std::thread::spawn(move || focus_guardian(hwnd));
        }
        // 低级键盘钩子需要独立线程 + 消息循环
        std::thread::spawn(keyboard_hook_thread);
    }

    // 4. 进程黑名单监控
    std::thread::spawn(process_watcher);

    log("kiosk activated");
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

#[cfg(windows)]
fn focus_guardian(hwnd: isize) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, VK_MENU, KEYEVENTF_KEYUP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsIconic, SetForegroundWindow, SetWindowPos, ShowWindow,
        HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_SHOWMAXIMIZED,
    };

    let target = hwnd as isize;
    loop {
        thread::sleep(Duration::from_millis(500));
        unsafe {
            let target_h: windows_sys::Win32::Foundation::HWND = target as *mut core::ffi::c_void;
            // 窗口被最小化则恢复
            if IsIconic(target_h) != 0 {
                ShowWindow(target_h, SW_SHOWMAXIMIZED);
            }
            // 焦点不在本窗口 → 用 Alt 键辅助夺回（绕过 SetForegroundWindow 限制）
            let fg = GetForegroundWindow();
            if fg != target_h {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                let _ = SetForegroundWindow(target_h);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            }
            // 重新钉 TOPMOST，防止其他窗口压顶
            SetWindowPos(
                target_h,
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

// ---------------- 低级键盘钩子 ----------------

#[cfg(windows)]
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
        if code >= 0 && (wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN) {
            let info: &KBDLLHOOKSTRUCT = &*(lparam as *const KBDLLHOOKSTRUCT);
            let vk = info.vkCode;
            let alt = (info.flags & LLKHF_ALTDOWN) != 0;
            let ctrl = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
            let shift = (GetAsyncKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0;

            let blocked = alt && vk == VK_TAB as u32           // Alt+Tab
                || alt && vk == VK_ESCAPE as u32               // Alt+Esc
                || alt && vk == VK_F4 as u32                   // Alt+F4
                || alt && vk == VK_SPACE as u32                // Alt+Space 系统菜单
                || vk == VK_LWIN as u32                        // Win 键
                || vk == VK_RWIN as u32                        // 右 Win 键
                || ctrl && shift && vk == VK_ESCAPE as u32     // Ctrl+Shift+Esc 任务管理器
                || ctrl && vk == VK_ESCAPE as u32              // Ctrl+Esc 开始菜单
                || vk == VK_F11 as u32;                        // F11 切全屏

            if blocked {
                log(&format!("blocked hotkey vkCode=0x{vk:x}"));
                return 1; // 吞掉按键
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
                // 同一进程名只告警一次，避免刷日志
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

        thread::sleep(Duration::from_secs(2));
    }
}
