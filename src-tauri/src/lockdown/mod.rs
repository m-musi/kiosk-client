//! Kiosk 锁定模块 —— 跨平台入口
//!
//! 职责（对应技术路线文档的四层架构）：
//! 1. 应用层：全屏无边框 + 置顶
//! 2. 输入锁定层：拦截 Alt+Tab / Win键 / Ctrl+Shift+Esc / Alt+F4 等切屏快捷键
//! 3. 外壳锁定层：焦点守护，焦点被夺走立即夺回
//! 4. 进程管控层：远控/通讯软件黑名单检测 + 处置

#[allow(dead_code)]
pub const DEFAULT_URL: &str = "https://sim.elecbalance.com/login?queryCode=1794879926545";

// ============ 启动安全门禁 ============
// 窗口隐藏状态下执行：检测分屏（多显示器）与远控软件，
// 不通过则弹窗（重试检测 / 退出系统），通过后调用方再显示窗口进入霸屏。

pub fn precheck_gate(window: &tauri::WebviewWindow) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

    loop {
        let mut problems: Vec<String> = Vec::new();

        // 1. 分屏检测：枚举显示器数量
        match window.available_monitors() {
            Ok(monitors) if monitors.len() > 1 => {
                problems.push(format!(
                    "检测到 {} 台显示器（分屏环境），请断开额外显示器或退出系统",
                    monitors.len()
                ));
            }
            Ok(_) => {}
            Err(e) => eprintln!("enumerate monitors failed: {e}"),
        }

        // 2. 远控软件检测
        let mut sys = sysinfo::System::new_all();
        sys.refresh_processes();
        let found_remote: Vec<String> = sys
            .processes()
            .iter()
            .filter(|(pid, p)| {
                pid.as_u32() != std::process::id()
                    && name_matches(&p.name().to_lowercase(), REMOTE_CONTROL_APPS)
            })
            .map(|(_, p)| p.name().to_string())
            .collect();

        if !found_remote.is_empty() {
            problems.push(format!(
                "检测到远程控制软件：{}，请关闭软件后重试",
                found_remote.join("、")
            ));
        }

        // 全部通过 → 放行
        if problems.is_empty() {
            return true;
        }

        // 弹窗：重试检测 / 退出系统
        let text = format!(
            "安全检测未通过，无法进入本系统：\n\n{}\n\n请关闭相关软件或断开分屏后点击「重试检测」；点击「退出系统」将关闭本程序。",
            problems.join("\n\n")
        );
        let retry = window
            .dialog()
            .message(text)
            .title("安全检测")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "重试检测".to_string(),
                "退出系统".to_string(),
            ))
            .blocking_show();

        if !retry {
            return false;
        }
        // 选了重试 → 继续下一轮检测
    }
}

/// 进程名黑名单匹配（供各平台模块与门禁共用）
pub fn name_matches(process_name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|p| process_name.contains(p))
}

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::{activate, precheck_and_log};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{activate, precheck_and_log};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use other_impl::activate;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod other_impl {
    use tauri::WebviewWindow;

    pub fn activate(window: &WebviewWindow) {
        let _ = window.set_fullscreen(true);
        let _ = window.set_always_on_top(true);
    }

    pub fn precheck_and_log() {}
}

// ============ 共享：进程黑名单特征库 ============
// 注意：生产环境建议改为云端特征库热更新（见技术路线文档第六节）

/// 远程控制软件（高危：直接绕过霸屏，默认自动终止）
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

/// 通讯软件（中危：可弹窗分散注意力，默认仅告警不终止）
pub const COMMUNICATION_APPS: &[&str] = &[
    "wechat",
    "weixin",
    "wxwork",        // 企业微信
    "wework",
    "qq",
    "tim",
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
pub const AUTO_KILL_COMMUNICATION: bool = true;
