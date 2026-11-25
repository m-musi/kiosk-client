//! macOS 平台锁定实现
//!
//! 核心：NSApplicationPresentationOptions（AppKit 系统级 Kiosk API），等效于 Swift：
//! ```swift
//! NSApp.presentationOptions = [
//!     .hideDock,                    // Dock 彻底隐藏且禁用
//!     .hideMenuBar,                 // 菜单栏隐藏
//!     .disableAppleMenu,            // Apple 菜单禁用
//!     .disableProcessSwitching,     // Cmd+Tab 禁用
//!     .disableForceQuit,            // Cmd+Opt+Esc 强制退出面板禁用
//!     .disableSessionTermination,   // 电源键重启/关机面板禁用
//!     .disableHideApplication,
//!     .disableMissionControl,       // Mission Control（调度中心）禁用
//!     .disableNotificationCenter,   // 通知中心禁用
//!     .fullScreen
//! ]
//! ```
//!
//! ⚠️ 关键特性：这组选项**只在应用处于激活（frontmost）状态时被系统执行**。
//! 因此除了设置选项，还必须有「前台守护」：周期性检查 frontmostApplication，
//! 若不是本进程，立即 activateIgnoringOtherApps 夺回前台并重新断言选项。

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use objc2::class;
use objc2::msg_send;
use objc2::runtime::AnyObject;

use tauri::WebviewWindow;

use super::{
    name_matches, COMMUNICATION_APPS, AUTO_KILL_COMMUNICATION, AUTO_KILL_REMOTE,
    REMOTE_CONTROL_APPS,
};

/// Kiosk 演示模式位掩码（NSApplicationPresentationOptions，AppKit 头文件定义）
const OPT_HIDE_DOCK: u64 = 1 << 1;
const OPT_HIDE_MENU_BAR: u64 = 1 << 3;
const OPT_DISABLE_HIDE_APPLICATION: u64 = 1 << 4;
const OPT_DISABLE_APPLE_MENU: u64 = 1 << 6;
const OPT_DISABLE_PROCESS_SWITCHING: u64 = 1 << 7; // 禁 Cmd+Tab
const OPT_DISABLE_FORCE_QUIT: u64 = 1 << 8; // 禁 Cmd+Opt+Esc
const OPT_DISABLE_SESSION_TERMINATION: u64 = 1 << 9; // 禁电源键重启/关机面板
const OPT_DISABLE_MISSION_CONTROL: u64 = 1 << 11;
const OPT_DISABLE_NOTIFICATION_CENTER: u64 = 1 << 12;
const OPT_FULL_SCREEN: u64 = 1 << 15;

const PRESENTATION_KIOSK: u64 = OPT_HIDE_DOCK
    | OPT_HIDE_MENU_BAR
    | OPT_DISABLE_HIDE_APPLICATION
    | OPT_DISABLE_APPLE_MENU
    | OPT_DISABLE_PROCESS_SWITCHING
    | OPT_DISABLE_FORCE_QUIT
    | OPT_DISABLE_SESSION_TERMINATION
    | OPT_DISABLE_MISSION_CONTROL
    | OPT_DISABLE_NOTIFICATION_CENTER
    | OPT_FULL_SCREEN;

/// 轻量文件日志，写入 /tmp/elecbalance-kiosk.log，便于在用户机器上验证锁定状态
fn klog(msg: &str) {
    let path = std::path::PathBuf::from("/tmp/elecbalance-kiosk.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/// 主线程调用：设置 Kiosk 演示模式选项，并回读校验
fn apply_presentation_options() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            klog("apply_presentation_options: sharedApplication is null");
            return;
        }
        let _: () = msg_send![app, setPresentationOptions: PRESENTATION_KIOSK];
        let cur: u64 = msg_send![app, presentationOptions];
        if cur != PRESENTATION_KIOSK {
            klog(&format!(
                "presentationOptions mismatch: want={PRESENTATION_KIOSK} got={cur}"
            ));
        }
    }
}

/// 当前前台应用的 PID（0 = 未知）
unsafe fn frontmost_pid() -> i32 {
    let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    if ws.is_null() {
        return 0;
    }
    let front: *mut AnyObject = msg_send![ws, frontmostApplication];
    if front.is_null() {
        return 0;
    }
    let pid: i32 = msg_send![front, processIdentifier];
    pid
}

/// 主线程调用：确保本应用处于前台，否则夺回并重新断言 Kiosk 选项
fn kiosk_tick() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let front = frontmost_pid();
        if front != 0 && front != std::process::id() as i32 {
            klog(&format!("frontmost stolen by pid {front}, reclaiming"));
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
        apply_presentation_options();
    }
}

pub fn activate(window: &WebviewWindow) {
    // 窗口层：全屏 + 无边框 + 置顶
    let _ = window.set_fullscreen(true);
    let _ = window.set_decorations(false);
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    super::LOCK_ACTIVE.store(true, Ordering::SeqCst);

    // 立即激活应用并设置 Kiosk 选项（必须在主线程）
    let _ = window.run_on_main_thread(|| {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, activateIgnoringOtherApps: true];
            }
        }
        apply_presentation_options();
        klog(&format!(
            "kiosk activated, pid={}, options={PRESENTATION_KIOSK}",
            std::process::id()
        ));
    });

    precheck_and_log();

    // 前台守护：每 2s 检查 frontmost 是否被抢占，被抢则夺回 + 重新断言选项
    let win = window.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        if !super::LOCK_ACTIVE.load(Ordering::SeqCst) {
            break;
        }
        let _ = win.run_on_main_thread(kiosk_tick);
    });

    // 进程监控（跨平台 sysinfo）
    thread::spawn(process_watcher);
}

/// 退出考试：清除 Kiosk 演示选项，回到普通桌面
pub fn deactivate(window: &WebviewWindow) {
    super::LOCK_ACTIVE.store(false, Ordering::SeqCst);
    let _ = window.run_on_main_thread(|| unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, setPresentationOptions: 0u64];
        }
    });
    klog("kiosk deactivated, returned to guide");
}

pub fn precheck_and_log() {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_processes();
    let mut found = Vec::new();
    for (_pid, process) in sys.processes() {
        let name = process.name().to_lowercase();
        if name_matches(&name, REMOTE_CONTROL_APPS) {
            found.push(name.clone());
        } else if name_matches(&name, COMMUNICATION_APPS) {
            found.push(name.clone());
        }
    }
    if !found.is_empty() {
        eprintln!("precheck: 检测到违规软件 {:?}", found);
        klog(&format!("precheck: found {:?}", found));
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
            if name_matches(&name, REMOTE_CONTROL_APPS) && AUTO_KILL_REMOTE {
                eprintln!("killing remote-control process: {name} (pid {pid})");
                let _ = process.kill();
            } else if name_matches(&name, COMMUNICATION_APPS) && AUTO_KILL_COMMUNICATION {
                eprintln!("killing communication process: {name} (pid {pid})");
                let _ = process.kill();
            }
        }
    }
}
