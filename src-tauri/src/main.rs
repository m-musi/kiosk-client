// 防止 release 构建弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod lockdown;
#[cfg(target_os = "windows")]
mod fab_win;

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

/// 引导页实时轮询：检测即终止远控进程，返回清理结果
#[tauri::command]
fn check_remote_software() -> serde_json::Value {
    let (killed, apps) = lockdown::kill_remote_apps();
    if !killed.is_empty() {
        exam_log(&format!("检测阶段已终止远控进程：{}", killed.join("、")));
    }
    serde_json::json!({ "blocked": !apps.is_empty(), "apps": apps, "killed": killed })
}

/// 进入考试：复核环境 → 打开考试窗口 → 副屏遮挡 → 悬浮退出按钮 → 霸屏锁定
/// 必须为 async：同步命令跑在主线程上，窗口 build() 会派发回主线程造成死锁（Windows 已知问题）
#[tauri::command]
async fn enter_exam(app: tauri::AppHandle) -> Result<(), String> {
    exam_log("enter_exam start");
    let (killed, apps) = lockdown::kill_remote_apps();
    if !killed.is_empty() {
        exam_log(&format!("进入考试前已终止远控进程：{}", killed.join("、")));
    }
    if !apps.is_empty() {
        return Err(format!("远控进程无法终止，请手动退出：{}", apps.join("、")));
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
    let (px, py, ph, scale) = match &primary {
        Some(m) => (
            m.position().x,
            m.position().y,
            m.size().height as i32,
            m.scale_factor(),
        ),
        None => (0, 0, 768, 1.0),
    };
    let margin = 20.0_f64; // 距屏幕左/下各 20 物理像素

    #[cfg(target_os = "windows")]
    {
        // Windows：原生分层窗口自绘抗锯齿圆按钮。窗口=按钮尺寸、逐像素透明，
        // 不受 WebView2 最小窗口约束，也不会露出矩形底色块。
        let side = (48.0_f64 * scale).round() as i32;
        let fab_x = px + margin as i32;
        let fab_y = py + ph - margin as i32 - side;
        fab_win::create(app.clone(), fab_x, fab_y, side);
        exam_log(&format!(
            "native fab created pos=({fab_x},{fab_y}) side={side} scale={scale}"
        ));
    }

    #[cfg(not(target_os = "windows"))]
    {
        // macOS 基线：透明 WebView 窗口 + CSS 圆形按钮，一行未改。
        let fab_size = 48.0_f64;
        let win_h_physical = fab_size * scale;
        let fab_x = (px as f64 + margin).round() as i32;
        let fab_y = ((py + ph) as f64 - margin - win_h_physical).round() as i32;
        let fab_pos = tauri::PhysicalPosition::new(fab_x, fab_y);
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
        .accept_first_mouse(true)
        .inner_size(fab_size, fab_size)
        .position(fab_x as f64 / scale, fab_y as f64 / scale)
        .build();
        if let Ok(w) = built {
            let _ = w.set_position(fab_pos);
            #[cfg(target_os = "macos")]
            unsafe {
                use objc2::msg_send;
                use objc2::runtime::AnyObject;
                if let Ok(ns_win) = w.ns_window() {
                    let ns_win = ns_win as *mut AnyObject;
                    let _: () = msg_send![ns_win, setAcceptsMouseMovedEvents: true];
                }
            }
            let _ = w.set_cursor_icon(tauri::CursorIcon::Hand);
        }
        exam_log(&format!(
            "fab window done pos=({fab_x},{fab_y}) scale={scale}"
        ));
    }

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

/// 打开退出二次确认窗（async 同上，避免主线程死锁）
pub(crate) async fn open_exit_confirm(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("exit-confirm") {
        // 复用已创建的窗口。取消时只是 hide，必须显式 show 才会重新显示；
        // 早期版本只调 set_focus 就 return，隐藏态窗口不会重现，表现为「点了没反应」。
        let _ = w.show();
        let _ = w.set_focus();
        exam_log("exit confirm shown (reuse)");
        return;
    }
    let built = WebviewWindowBuilder::new(
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
    match built {
        Ok(w) => {
            // 关键点：kiosk 演示模式下新建窗口不会自动获得前台可见性，
            // 必须显式 show + focus，否则第一次点击「没反应」，要点第二次才显示。
            let _ = w.show();
            let _ = w.set_focus();
            exam_log("exit confirm created + shown");
        }
        Err(e) => exam_log(&format!("exit confirm build failed: {e}")),
    }
}

#[tauri::command]
async fn show_exit_confirm(app: tauri::AppHandle) {
    open_exit_confirm(app).await;
}

/// 取消退出：隐藏确认窗（不销毁），焦点还给考试窗口。
/// 不能改用 close()：close 是异步销毁，紧接着再点悬浮按钮时会命中「正在关闭」的
/// 句柄，show_exit_confirm 只 set_focus 不 show 也不重建，导致按钮失灵。
#[tauri::command]
fn cancel_exit_exam(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("exit-confirm") {
        let _ = w.hide();
    }
    if let Some(exam) = app.get_webview_window("exam") {
        let _ = exam.set_focus();
    }
    exam_log("exit confirm cancelled (hidden)");
}

/// 清空内嵌考试网页在本机留下的所有存储数据。
///
/// 覆盖 exam 窗口和所有副屏遮挡窗口（block-*，加载同一考试站点）：
/// 1. eval 同步清 localStorage/sessionStorage —— 页面内立即生效，作为兜底；
/// 2. clear_all_browsing_data 清 cookies、IndexedDB、HTTP 缓存、service worker
///    （Windows 走 ICoreWebView2Profile2::ClearBrowsingDataAll，删除是异步落盘的，
///    调用后需短暂等待再关窗，否则删除可能被窗口销毁中止）。
fn clear_exam_web_storage(app: &tauri::AppHandle) {
    for (label, w) in app.webview_windows() {
        if label == "exam" || label.starts_with("block-") {
            let _ = w.eval("try{localStorage.clear();sessionStorage.clear();}catch(e){}");
            if let Err(e) = w.clear_all_browsing_data() {
                exam_log(&format!("clear browsing data({label}) failed: {e}"));
            } else {
                exam_log(&format!("clear browsing data({label}) requested"));
            }
        }
    }
}

/// 确认退出：解除锁定、清空考试页本地存储、关闭考试相关窗口，回到引导页
#[tauri::command]
async fn confirm_exit_exam(app: tauri::AppHandle) {
    if let Some(exam) = app.get_webview_window("exam") {
        lockdown::deactivate(&exam);
    }

    // Windows 的悬浮按钮是原生分层窗口（不在 Tauri 窗口表里），必须显式销毁，
    // 否则退出考试后按钮会残留在屏幕上。
    #[cfg(target_os = "windows")]
    fab_win::destroy();

    clear_exam_web_storage(&app);
    // 浏览数据删除是异步的，留 300ms 落盘后再销毁 webview
    tauri::async_runtime::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
    })
    .await
    .ok();

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
        clear_exam_web_storage(&app);
        // 给异步的浏览数据删除留出落盘时间
        std::thread::sleep(std::time::Duration::from_millis(300));
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
