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
//! 通过 objc2 的 msg_send 以 NSUInteger 位掩码调用，避免依赖生成的类型绑定。

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

/// 在主线程调用 NSApplication setPresentationOptions
pub fn apply_presentation_options() {
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let _: () = msg_send![app, setPresentationOptions: PRESENTATION_KIOSK];
    }
}

pub fn activate(window: &WebviewWindow) {
    // 窗口层：全屏 + 无边框 + 置顶
    let _ = window.set_fullscreen(true);
    let _ = window.set_decorations(false);
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    // 系统层：Kiosk 演示模式（禁 Dock / Cmd+Tab / 强退面板等）
    apply_presentation_options();

    precheck_and_log();

    // presentationOptions 会在应用失焦等场景被系统重置，每 5s 在主线程重新断言
    let win = window.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(5));
        let _ = win.run_on_main_thread(apply_presentation_options);
    });

    // 进程监控（跨平台 sysinfo）
    thread::spawn(process_watcher);
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
            if name_matches(&name, REMOTE_CONTROL_APPS) && AUTO_KILL_REMOTE {
                eprintln!("killing remote-control process: {name} (pid {pid})");
                let _ = process.kill();
            } else if name_matches(&name, COMMUNICATION_APPS) && AUTO_KILL_COMMUNICATION {
                eprintln!("killing communication process: {name} (pid {pid})");
                let _ = process.kill();
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}
