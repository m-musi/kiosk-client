//! Kiosk 锁定模块 —— 跨平台入口
//!
//! 职责：
//! 1. 引导页阶段：实时检测远程控制软件，决定是否允许进入考试
//! 2. 应用层：全屏无边框 + 置顶
//! 3. 输入锁定层：拦截 Alt+Tab / Win 键 / Ctrl+Shift+Esc / Alt+F4 等切屏快捷键
//! 4. 外壳锁定层：焦点守护，焦点被夺走立即夺回
//! 5. 进程管控层：远控/通讯软件黑名单检测 + 处置

use std::sync::atomic::AtomicBool;

/// 考试页面地址（唯一配置点）
pub const EXAM_URL: &str = "https://sim.elecbalance.com/login";

/// 当前是否处于霸屏锁定状态。引导页与键盘钩子/守护线程共用。
pub static LOCK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 自动更新总开关。当前未启用签名更新（pubkey 为空、createUpdaterArtifacts=false），
/// 打开会在启动时阻塞网络请求，导致引导页无法显示。启用签名更新前保持 false。
pub const UPDATER_ENABLED: bool = false;

// ============ 管理员退出 ============

/// 管理员退出热键（Ctrl+Alt+Shift+Q，未被键盘钩子拦截）
pub const ADMIN_EXIT_HOTKEY: &str = "ctrl+alt+shift+q";

/// 管理员退出口令（修改后重新打包生效）
pub const ADMIN_PASSWORD: &str = "admin123";

// ============ 远控检测（引导页实时轮询 + 进入考试前复核） ============

/// 返回当前正在运行的远程控制软件进程名（去重）
pub fn running_remote_apps() -> Vec<String> {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_processes();
    let mut found: Vec<String> = sys
        .processes()
        .iter()
        .filter(|(pid, p)| {
            pid.as_u32() != std::process::id()
                && name_matches(&p.name().to_lowercase(), REMOTE_CONTROL_APPS)
        })
        .map(|(_, p)| p.name().to_string())
        .collect();
    found.sort();
    found.dedup();
    found
}

/// 终止所有命中黑名单的远程控制软件进程，并在短暂等待后复查。
/// 返回 (成功发起终止的进程名, 复查后仍在运行的进程名)。
/// 注意：必须排除自身 PID，避免把客户端自己结束掉。
pub fn kill_remote_apps() -> (Vec<String>, Vec<String>) {
    let mut killed = Vec::new();
    let sys = sysinfo::System::new_all();
    let self_pid = std::process::id();
    for (pid, p) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        let name = p.name().to_string();
        if name_matches(&name.to_lowercase(), REMOTE_CONTROL_APPS) && p.kill() {
            killed.push(name);
        }
    }
    killed.sort();
    killed.dedup();

    if !killed.is_empty() {
        // 给被终止的进程一点退出时间，避免刚发出信号就误判为仍在运行
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    (killed, running_remote_apps())
}

/// 进程名黑名单匹配（供各平台模块与引导页共用）
pub fn name_matches(process_name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| process_name.contains(p))
}

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::{activate, deactivate, precheck_and_log};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use macos::{activate, deactivate, precheck_and_log};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use other_impl::{activate, deactivate};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod other_impl {
    use std::sync::atomic::Ordering;
    use tauri::WebviewWindow;

    pub fn activate(window: &WebviewWindow) {
        let _ = window.set_fullscreen(true);
        let _ = window.set_always_on_top(true);
        super::LOCK_ACTIVE.store(true, Ordering::SeqCst);
    }

    pub fn deactivate(_window: &WebviewWindow) {
        super::LOCK_ACTIVE.store(false, Ordering::SeqCst);
    }
}

// ============ 共享：进程黑名单特征库 ============

/// 远程控制软件（高危：检测到即终止）
pub const REMOTE_CONTROL_APPS: &[&str] = &[
    "todesk",
    "sunlogin",      // 向日葵
    "sunflower",     // 向日葵旧进程名
    "oray",          // 向日葵厂商
    "teamviewer",
    "anydesk",
    "rustdesk",
    "splashtop",
    "getscreen",
    "airdroid",
    "deskin",
    "gostream",
    "iexplore",
];

/// 通讯软件（中危：可弹窗分散注意力，默认自动终止，按需调整）
pub const COMMUNICATION_APPS: &[&str] = &[
    "wechat",
    "weixin",
    "wxwork",        // 企业微信
    "wework",
    "qq.exe",        // 用完整进程名，避免误匹配 RuntimeBroker 等系统进程
    "tim.exe",       // 同上
    "dingtalk",
    "feishu",
    "lark",
    "telegram",
    "discord",
    "slack",
];

/// 是否自动终止远控类进程（true = 检测到即杀）
pub const AUTO_KILL_REMOTE: bool = true;
/// 是否自动终止通讯类进程（false = 仅记录日志与告警）
pub const AUTO_KILL_COMMUNICATION: bool = false;
