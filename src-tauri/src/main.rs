// 防止 release 构建弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod lockdown;

use tauri::Manager;

/// 自动更新：检查云端版本 → 下载 → 安装 → 重启
/// 时序：在窗口隐藏、安全门禁之前执行，更新后重启会重新走一遍启动流程。
/// 若 tauri.conf.json 中未配置有效 pubkey/endpoints，这里会静默跳过。
fn check_for_updates(app: &tauri::AppHandle) {
    use tauri_plugin_updater::UpdaterExt;

    let result = tauri::async_runtime::block_on(async {
        let updater = match app.updater_builder().build() {
            Ok(u) => u,
            Err(e) => return Err(format!("updater not configured: {e}")),
        };
        let update = match updater.check().await {
            Ok(Some(u)) => u,
            Ok(None) => return Ok(()), // 已是最新版本
            Err(e) => return Err(e.to_string()),
        };
        eprintln!("发现新版本 {}，开始下载...", update.version);
        if let Err(e) = update
            .download_and_install(
                |chunk, total| match total {
                    Some(t) => eprintln!("下载中 {chunk}/{t}"),
                    None => eprintln!("下载中 {chunk}"),
                },
                || eprintln!("下载完成，开始安装..."),
            )
            .await
        {
            return Err(e.to_string());
        }
        eprintln!("更新完成，正在重启...");
        app.restart(); // never returns
    });

    if let Err(e) = result {
        eprintln!("自动更新检查失败（忽略，继续正常启动）: {e}");
    }
}

fn main() {
    tauri::Builder::default()
        // 单实例：重复启动直接聚焦已有窗口
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main window not found in tauri.conf.json");

            // 管理员退出热键：呼出口令输入窗（系统级全局热键，无需应用在前台）
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            app.global_shortcut()
                .on_shortcut(lockdown::ADMIN_EXIT_HOTKEY, |app, _shortcut, _event| {
                    show_exit_dialog(app);
                })?;

            // 窗口保持隐藏：先自动更新，再过安全门禁
            check_for_updates(app.handle());
            if !lockdown::precheck_gate(&window) {
                // 用户选择"退出系统"
                app.handle().exit(0);
                return Ok(());
            }

            // 门禁通过：显示窗口并进入霸屏锁定模式
            let _ = window.show();
            lockdown::activate(&window);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![admin_exit, admin_close])
        .run(tauri::generate_context!())
        .expect("error while running kiosk application");
}

/// 呼出管理员退出口令窗（已有则聚焦）
fn show_exit_dialog(app: &tauri::AppHandle) {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    if let Some(w) = app.get_webview_window("admin-exit") {
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "admin-exit", WebviewUrl::App("exit.html".into()))
        .title("管理员退出")
        .inner_size(400.0, 250.0)
        .resizable(false)
        .always_on_top(true)
        .center()
        .build();
    match built {
        Ok(w) => {
            // 口令窗关闭（取消/系统关闭按钮）后把焦点还给霸屏主窗口
            let main = app.get_webview_window("main");
            w.on_window_event(move |event| {
                if let tauri::WindowEvent::Destroyed = event {
                    if let Some(main) = &main {
                        let _ = main.set_focus();
                    }
                }
            });
        }
        Err(e) => eprintln!("open admin exit dialog failed: {e}"),
    }
}

/// 校验管理员口令：正确 → 解锁退出；错误 → 拒绝（每次错误延迟 1s 防暴力尝试）
#[tauri::command]
fn admin_exit(password: String, app: tauri::AppHandle) -> Result<(), String> {
    if password == lockdown::ADMIN_PASSWORD {
        eprintln!("admin exit: password accepted, shutting down");
        // 进程退出时键盘钩子、置顶、presentationOptions 随之自动失效，无需手动解锁
        app.exit(0);
        Ok(())
    } else {
        eprintln!("admin exit: WRONG password attempt");
        std::thread::sleep(std::time::Duration::from_secs(1)); // 拖慢暴力尝试
        Err("口令错误".into())
    }
}

/// 关闭口令窗并把焦点还给主窗口（取消退出）
#[tauri::command]
fn admin_close(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("admin-exit") {
        let _ = w.close();
    }
}
