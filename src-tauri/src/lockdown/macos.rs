//! macOS 平台锁定实现
//!
//! 当前为跨平台基础版（Tauri API 层），NSApplicationPresentationOptions 原生增强见 README。
//!
//! 原生 Kiosk 增强（阶段二，需 objc2-app-kit，等效 Swift 代码）：
//! ```swift
//! NSApp.presentationOptions = [
//!     .hideDock,                    // Dock 彻底隐藏且禁用
//!     .hideMenuBar,                 // 菜单栏隐藏
//!     .disableAppleMenu,            // Apple 菜单禁用
//!     .disableProcessSwitching,     // Cmd+Tab 禁用
//!     .disableForceQuit,            // Cmd+Opt+Esc 强制退出面板禁用
//!     .disableSessionTermination,   // 电源键重启/关机面板禁用
//!     .disableHideApplication,
//!     .fullScreen
//! ]
//! // 窗口移除关闭按钮（防 Cmd+W）：
//! window.styleMask = window.styleMask & ~NSWindowStyleMaskClosable
//! ```
//! Rust 侧调用路径：objc2-app-kit 的 NSApplication::sharedApplication()
//! 设 setPresentationOptions，或用 msg_send 调用。

use tauri::WebviewWindow;

use super::{
    name_matches, COMMUNICATION_APPS, AUTO_KILL_COMMUNICATION, AUTO_KILL_REMOTE,
    REMOTE_CONTROL_APPS,
};

pub fn activate(window: &WebviewWindow) {
    // 基础霸屏（全屏 + 无边框 + 置顶）
    let _ = window.set_fullscreen(true);
    let _ = window.set_decorations(false);
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    precheck_and_log();

    // 进程监控（跨平台 sysinfo）
    std::thread::spawn(process_watcher);
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
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
