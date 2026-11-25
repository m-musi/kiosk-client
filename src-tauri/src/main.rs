// 防止 release 构建弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod lockdown;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// 自动更新：检查云端版本 → 下载 → 安装 → 重启
/// 未配置有效 pubkey/endpoints 时静默跳过，不影响正常启动。
fn check_for_updates(app: &tauri::AppHandle) {
    // 当前为免签名内部包：更新服务器不可达时会长时间阻塞，直接跳过。
    if !lockdown::UPDATER_ENABLED {
        return;
    }
    use tauri_plugin_updater::UpdaterExt;

    let result = tauri::async_runtime::block_on(async {
        let updater = match app.updater_builder().build() {
            Ok(u) => u,
            Err(e) => return Err(format!("updater not configured: {e}")),
        };
        let update = match updater.check().await {
            Ok(Some(u)) => u,
            Ok(None) => return Ok(()),
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
        app.restart();
    });

    if let Err(e) = result {
        eprintln!("自动更新检查失败（忽略，继续正常启动）: {e}");
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            if let Some(guide) = app.get_webview_window("main") {
                let _ = guide.center();
                let _ = guide.set_focus();
            }

            // 管理员退出热键：呼出口令输入窗
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            if let Err(e) =
                app.global_shortcut()
                    .on_shortcut(lockdown::ADMIN_EXIT_HOTKEY, |app, _shortcut, _event| {
                        show_exit_dialog(app);
                    })
            {
                // 热键被占用不应导致整个客户端启动失败
                eprintln!("register admin exit hotkey failed: {e}");
            }

            spawn_update_check(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_remote_software,
            enter_exam,
            show_exit_confirm,
            cancel_exit_exam,
            confirm_exit_exam,
            exit_app,
            admin_exit,
            admin_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running kiosk application");
}

/// 更新检查放到后台线程，避免任何网络等待阻塞 UI 线程
fn spawn_update_check(app: tauri::AppHandle) {
    std::thread::spawn(move || check_for_updates(&app));
}

/// 进入考试流程诊断日志（release 无控制台，单独落盘）
fn exam_log(msg: &str) {
    use std::io::Write;
    let line = format!("[{}] {msg}\n", chrono_like_stamp());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("elecbalance-exam.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn chrono_like_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

/// 引导页实时轮询：返回是否有远程控制软件正在运行
#[tauri::command]
fn check_remote_software() -> serde_json::Value {
    let apps = lockdown::running_remote_apps();
    serde_json::json!({ "blocked": !apps.is_empty(), "apps": apps })
}

/// 进入考试：复核环境 → 打开考试窗口 → 副屏遮挡 → 悬浮退出按钮 → 霸屏锁定
/// 必须为 async：同步命令跑在主线程上，窗口 build() 会派发回主线程造成死锁（Windows 已知问题）
#[tauri::command]
async fn enter_exam(app: tauri::AppHandle) -> Result<(), String> {
    exam_log("enter_exam start");
    let apps = lockdown::running_remote_apps();
    if !apps.is_empty() {
        return Err(format!("请关闭远程软件：{}", apps.join("、")));
    }
    if let Some(exam) = app.get_webview_window("exam") {
        let _ = exam.set_focus();
        return Ok(());
    }

    let url = lockdown::EXAM_URL
        .parse::<url::Url>()
        .map_err(|_| "考试地址配置错误".to_string())?;

    exam_log("remote check passed, creating exam window");
    let exam = WebviewWindowBuilder::new(&app, "exam", WebviewUrl::External(url))
        .title("山西省电力仿真考试客户端")
        .inner_size(1366.0, 768.0)
        .decorations(false)
        .resizable(false)
        .visible(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    exam_log("exam window built");

    let monitors = exam.available_monitors().unwrap_or_default();
    let primary = exam.primary_monitor().ok().flatten();

    // 取消多屏幕检测：副屏直接整屏遮挡，提示在本机屏幕操作
    for (i, monitor) in monitors.iter().enumerate() {
        if primary
            .as_ref()
            .map(|p| p.name() == monitor.name())
            .unwrap_or(false)
        {
            continue;
        }
        let built = WebviewWindowBuilder::new(
            &app,
            format!("block-{i}"),
            WebviewUrl::App("block.html".into()),
        )
        .title("考试进行中")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .position(monitor.position().x as f64, monitor.position().y as f64)
        .inner_size(monitor.size().width as f64, monitor.size().height as f64)
        .build();
        if let Ok(w) = built {
            let _ = w.set_position(*monitor.position());
        }
    }

    exam_log("monitor overlays done");

    // 悬浮退出按钮：置于考试网页上层
    let (px, py, _pw, ph) = match &primary {
        Some(m) => (
            m.position().x,
            m.position().y,
            m.size().width as i32,
            m.size().height as i32,
        ),
        None => (0, 0, 1366, 768),
    };
    let built = WebviewWindowBuilder::new(
        &app,
        "exam-fab",
        WebviewUrl::App("exit-fab.html".into()),
    )
    .title("退出考试")
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .inner_size(72.0, 72.0)
    .position(px as f64, (py + ph - 96) as f64)
    .build();
    if let Ok(w) = built {
        let _ = w.set_position(tauri::PhysicalPosition::new(px, py + ph - 96));
    }
    exam_log("fab window done");

    if let Some(guide) = app.get_webview_window("main") {
        let _ = guide.hide();
    }
    let _ = exam.show();
    let _ = exam.set_fullscreen(true);
    let _ = exam.set_focus();
    exam_log("exam shown, activating lockdown");
    lockdown::activate(&exam);
    exam_log("lockdown activated");

    Ok(())
}

/// 悬浮按钮点击：打开退出二次确认窗（async 同上，避免主线程死锁）
#[tauri::command]
async fn show_exit_confirm(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("exit-confirm") {
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(
        &app,
        "exit-confirm",
        WebviewUrl::App("exam-exit-confirm.html".into()),
    )
    .title("退出考试确认")
    .inner_size(410.0, 220.0)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .center()
    .build();
}

/// 取消退出：关闭确认窗，焦点还给考试窗口
#[tauri::command]
fn cancel_exit_exam(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("exit-confirm") {
        let _ = w.close();
    }
    if let Some(exam) = app.get_webview_window("exam") {
        let _ = exam.set_focus();
    }
}

/// 确认退出：解除锁定、关闭考试相关窗口，回到引导页
#[tauri::command]
fn confirm_exit_exam(app: tauri::AppHandle) {
    if let Some(exam) = app.get_webview_window("exam") {
        lockdown::deactivate(&exam);
    }

    let labels: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|label| {
            label.starts_with("block-")
                || matches!(label.as_str(), "exam" | "exam-fab" | "exit-confirm")
        })
        .cloned()
        .collect();
    for label in labels {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.close();
        }
    }

    if let Some(guide) = app.get_webview_window("main") {
        let _ = guide.show();
        let _ = guide.set_focus();
    }
}

/// 引导页「退出」：关闭客户端
#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// 呼出管理员退出口令窗（已有则聚焦）
/// 热键回调在主线程事件循环内执行，窗口创建必须放到后台线程，否则 build() 死锁
fn show_exit_dialog(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(w) = app.get_webview_window("admin-exit") {
            let _ = w.set_focus();
            return;
        }
        let built = WebviewWindowBuilder::new(&app, "admin-exit", WebviewUrl::App("exit.html".into()))
            .title("管理员退出")
            .inner_size(400.0, 250.0)
            .resizable(false)
            .always_on_top(true)
            .center()
            .build();
        match built {
            Ok(w) => {
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
    });
}

/// 校验管理员口令：正确 → 解锁退出；错误 → 拒绝（每次错误延迟 1s 防暴力尝试）
#[tauri::command]
fn admin_exit(password: String, app: tauri::AppHandle) -> Result<(), String> {
    if password == lockdown::ADMIN_PASSWORD {
        eprintln!("admin exit: password accepted, shutting down");
        if let Some(exam) = app.get_webview_window("exam") {
            lockdown::deactivate(&exam);
        }
        app.exit(0);
        Ok(())
    } else {
        eprintln!("admin exit: WRONG password attempt");
        std::thread::sleep(std::time::Duration::from_secs(1));
        Err("口令错误".into())
    }
}

/// 关闭口令窗并把焦点还给引导页（取消退出）
#[tauri::command]
fn admin_close(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("admin-exit") {
        let _ = w.close();
    }
}
