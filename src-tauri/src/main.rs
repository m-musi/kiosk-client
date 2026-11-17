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
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main window not found in tauri.conf.json");

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
        .run(tauri::generate_context!())
        .expect("error while running kiosk application");
}
