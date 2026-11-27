// 防止 release 构建弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod lockdown;
#[cfg(target_os = "windows")]
mod fab_win;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
#[cfg(target_os = "macos")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
#[cfg(target_os = "macos")]
use tauri::webview::PageLoadEvent;

/// 自动更新：检查云端版本 → 下载 → 安装 → 重启
/// 未配置有效 pubkey/endpoints 时静默跳过，不影响正常启动。
fn check_for_updates(app: &tauri::AppHandle) {
    // 当前为免签名内部包：更新服务器不可达时会长时间阻塞，直接跳过。
    if !lockdown::UPDATER_ENABLED {
        updater_log("updater disabled by KIOSK_UPDATER, skip");
        return;
    }
    use tauri_plugin_updater::UpdaterExt;

    updater_log(&format!("check start, app version={}", app.package_info().version));

    let result = tauri::async_runtime::block_on(async {
        let updater = match app.updater_builder().build() {
            Ok(u) => u,
            Err(e) => return Err(format!("updater not configured: {e}")),
        };
        let update = match updater.check().await {
            Ok(Some(u)) => {
                updater_log(&format!("update available: {}", u.version));
                u
            }
            Ok(None) => {
                updater_log("no update available");
                return Ok(());
            }
            Err(e) => return Err(format!("check failed: {e}")),
        };
        updater_log("start download...");
        if let Err(e) = update
            .download_and_install(
                |chunk, total| match total {
                    Some(t) => updater_log(&format!("download {chunk}/{t}")),
                    None => updater_log(&format!("download {chunk}")),
                },
                || updater_log("download done, installing..."),
            )
            .await
        {
            return Err(format!("install failed: {e}"));
        }
        updater_log("update installed, restarting...");
        app.restart();
    });

    if let Err(e) = result {
        updater_log(&format!("update aborted: {e}"));
    }
}

/// 更新流程日志（release 无控制台，落盘到临时目录便于排查）
fn updater_log(msg: &str) {
    use std::io::Write;
    let line = format!("[{}] [updater] {msg}\n", chrono_like_stamp());
    eprintln!("{}", line.trim_end());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("elecbalance-updater.log"))
    {
        let _ = f.write_all(line.as_bytes());
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

            // 启动时探测一次刘海高度：既提前验证公开 API 调用安全，也便于诊断。
            #[cfg(target_os = "macos")]
            exam_log(&format!("startup notch top = {}pt", notch_top_logical()));

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
            get_app_env,
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

/// 读取刘海（notch）占用的顶部安全区高度，单位：逻辑像素。
/// 用 NSScreen.safeAreaInsets（macOS 11+ 公开 API），非刘海屏返回 0。
#[cfg(target_os = "macos")]
fn notch_top_logical() -> f64 {
    use objc2::class;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    use objc2::encode::{Encode, Encoding};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSEdgeInsets {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    }

    // NSScreen.safeAreaInsets 返回 NSEdgeInsets，需要告诉 objc2 它的类型编码，
    // 否则 msg_send! 无法把返回值转成该结构体。
    unsafe impl Encode for NSEdgeInsets {
        const ENCODING: Encoding = Encoding::Struct(
            "NSEdgeInsets",
            &[
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
                f64::ENCODING,
            ],
        );
    }

    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            return 0.0;
        }
        // safeAreaInsets 在 macOS 11 以下不存在，先探测避免崩溃
        let sel = objc2::sel!(safeAreaInsets);
        let responds: bool = msg_send![screen, respondsToSelector: sel];
        if !responds {
            return 0.0;
        }
        let insets: NSEdgeInsets = msg_send![screen, safeAreaInsets];
        if insets.top.is_finite() && insets.top > 0.0 {
            insets.top
        } else {
            0.0
        }
    }
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
    let url = lockdown::EXAM_URL
        .parse::<url::Url>()
        .map_err(|_| "考试地址配置错误".to_string())?;
    #[cfg(target_os = "macos")]
    let retry_url = url.clone();

    // macOS：退出考试时只隐藏窗口、不清毁 WKWebView。再次进入直接复用，
    // 避免清理浏览数据后新建 WKWebView 首屏不渲染（白屏）。
    #[cfg(target_os = "macos")]
    if let Some(exam) = app.get_webview_window("exam") {
        exam_log("reenter exam: reuse existing exam window");
        if let Err(e) = exam.navigate(url.clone()) {
            exam_log(&format!("reenter navigate failed: {e}"));
        }
        if let Some(guide) = app.get_webview_window("main") {
            let _ = guide.hide();
        }
        for (label, w) in app.webview_windows() {
            if label == "exam"
                || label == "exam-fab"
                || label == "exam-backdrop"
                || label.starts_with("block-")
            {
                let _ = w.show();
            }
        }
        let _ = exam.set_focus();
        if let Some(m) = app.primary_monitor().ok().flatten() {
            let notch = notch_top_logical();
            let _ = exam.set_position(tauri::PhysicalPosition::new(
                m.position().x,
                m.position().y + (notch * m.scale_factor()).round() as i32,
            ));
            let _ = exam.set_size(tauri::PhysicalSize::new(
                m.size().width,
                m.size().height - (notch * m.scale_factor()).round() as u32,
            ));
        }
        lockdown::activate(&exam);
        // activate 会把 exam 置前，显式把悬浮按钮再 show 一次保证仍在最上层
        if let Some(fab) = app.get_webview_window("exam-fab") {
            let _ = fab.show();
            let _ = fab.set_focus();
        }
        exam_log("reentered exam, lockdown reactivated");
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    if let Some(exam) = app.get_webview_window("exam") {
        let _ = exam.set_focus();
        return Ok(());
    }

    let primary = app.primary_monitor().ok().flatten();
    let (mx, my, mw, mh, scale) = match &primary {
        Some(m) => (
            m.position().x,
            m.position().y,
            m.size().width,
            m.size().height,
            m.scale_factor(),
        ),
        None => (0, 0, 1366, 768, 1.0),
    };

    // macOS release 下，1366x768 的隐藏窗口切全屏时 WKWebView 不跟随重排，
    // 首屏只绘制左上角一小块，右键刷新才恢复。改为按主屏实际尺寸创建且立即可见，
    // 让 WebView 在页面加载时就是最终尺寸。
    // 刘海屏：窗口下沿对齐屏幕底部，上沿避开刘海，高度相应缩短，
    // 顶部露出的缺口用黑色底衬窗口补齐（见 exam-backdrop），避免露出桌面。
    #[cfg(target_os = "macos")]
    let notch = notch_top_logical();
    #[cfg(not(target_os = "macos"))]
    let notch = 0.0_f64;
    #[cfg(target_os = "macos")]
    let (exam_w, exam_h, exam_visible) = (mw as f64 / scale, mh as f64 / scale - notch, true);
    #[cfg(not(target_os = "macos"))]
    let (exam_w, exam_h, exam_visible) = (1366.0_f64, 768.0_f64, false);
    let exam_y = my as f64 / scale + notch;

    exam_log(&format!(
        "exam window init: monitor=({mx},{my},{mw}x{mh}) scale={scale} notch={notch} size=({exam_w}x{exam_h}) y={exam_y} visible={exam_visible}"
    ));

    if exam_visible {
        if let Some(guide) = app.get_webview_window("main") {
            let _ = guide.hide();
        }
    }

    #[cfg(target_os = "macos")]
    let page_loaded = Arc::new(AtomicBool::new(false));
    #[cfg(target_os = "macos")]
    let page_loaded_for_cb = page_loaded.clone();

    // 刘海屏底衬： exam 窗口为避开刘海从 y=notch 开始，顶部会露出一条桌面。
    // 用一张纯黑无边框窗口铺满整屏垫在下面，把这条缺口补成黑色。
    // 非刘海屏（notch=0）也创建，让它盖住菜单栏区域，视觉更干净。
    #[cfg(target_os = "macos")]
    if app.get_webview_window("exam-backdrop").is_none() {
        let built = WebviewWindowBuilder::new(
            &app,
            "exam-backdrop",
            WebviewUrl::App("backdrop.html".into()),
        )
        .title("")
        .decorations(false)
        .resizable(false)
        .shadow(false)
        .skip_taskbar(true)
        .position(mx as f64 / scale, my as f64 / scale)
        .inner_size(mw as f64 / scale, mh as f64 / scale)
        .build();
        if let Ok(w) = built {
            let _ = w.set_position(tauri::PhysicalPosition::new(mx, my));
            let _ = w.set_size(tauri::PhysicalSize::new(mw, mh));
            let _ = w.show();
            exam_log("exam-backdrop created + shown");
        }
    }

    exam_log("remote check passed, creating exam window");
    let mut builder = WebviewWindowBuilder::new(&app, "exam", WebviewUrl::External(url))
        .title("山西省电力仿真考试客户端")
        .inner_size(exam_w, exam_h)
        .decorations(false)
        .resizable(false)
        .visible(exam_visible)
        .position(mx as f64 / scale, exam_y);

    #[cfg(target_os = "macos")]
    {
        // 无边框窗口默认自带一圈阴影，刘海避让后 exam 顶边露在屏幕内会显示这条边框，关掉。
        builder = builder.shadow(false);
        builder = builder.on_page_load(move |_, payload| {
            let event = match payload.event() {
                PageLoadEvent::Started => "started",
                PageLoadEvent::Finished => {
                    page_loaded_for_cb.store(true, Ordering::SeqCst);
                    "finished"
                }
            };
            exam_log(&format!("exam page {event}: {}", payload.url()));
        });
    }

    let exam = builder.build().map_err(|e| e.to_string())?;
    exam_log("exam window built");

    let monitors = exam.available_monitors().unwrap_or_default();

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
            // Kiosk 演示选项下新建窗口可能不自动可见，显式 show（确认窗曾踩过同一坑）
            let _ = w.show();
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
    let _ = exam.set_focus();

    // macOS 不再切原生全屏：Full Screen 切换会让 WKWebView 偶发停止绘制，
    // 表现为白屏/右上角黑底，右键刷新才恢复。无边框窗口已按主屏尺寸铺满，
    // 配合 kiosk 演示选项（隐藏菜单栏/Dock、禁用切换）视觉与锁定效果一致。
    #[cfg(not(target_os = "macos"))]
    let _ = exam.set_fullscreen(true);
    #[cfg(target_os = "macos")]
    if let Some(m) = &primary {
        let notch = notch_top_logical();
        let _ = exam.set_position(tauri::PhysicalPosition::new(
            m.position().x,
            m.position().y + (notch * m.scale_factor()).round() as i32,
        ));
        let _ = exam.set_size(tauri::PhysicalSize::new(
            m.size().width,
            m.size().height - (notch * m.scale_factor()).round() as u32,
        ));
    }
    exam_log(&format!(
        "exam shown, inner_size={:?} notch={notch}",
        exam.inner_size()
    ));

    // 兜底：若首屏始终没有 Finished 事件，分别在 2.5s / 6s 重新导航一次。
    // 用 navigate 而不是 reload，避免首次导航尚未真正开始时 reload 落空。
    #[cfg(target_os = "macos")]
    {
        use std::time::Duration;
        let watch = exam.clone();
        let loaded = page_loaded.clone();
        std::thread::spawn(move || {
            let mut waited = 0u64;
            for (i, step) in [2500u64, 3500u64].iter().enumerate() {
                std::thread::sleep(Duration::from_millis(*step));
                waited += *step;
                if loaded.load(Ordering::SeqCst) {
                    exam_log(&format!(
                        "exam page loaded before retry {}, waited={}ms",
                        i + 1,
                        waited
                    ));
                    return;
                }
                exam_log(&format!(
                    "exam page not loaded, retry {} by navigate, waited={}ms",
                    i + 1,
                    waited
                ));
                let watch_main = watch.clone();
                let target = retry_url.clone();
                let _ = watch.run_on_main_thread(move || {
                    let _ = watch_main.navigate(target);
                });
            }
        });
    }

    exam_log("exam shown, activating lockdown");
    lockdown::activate(&exam);
    exam_log("lockdown activated");

    // Kiosk 演示选项切换后窗口层级可能重排，显式把悬浮按钮提到最上层。
    // 复用窗口与首次创建窗口两条路径都要保证按钮可见。
    if let Some(fab) = app.get_webview_window("exam-fab") {
        let _ = fab.show();
    }
    exam_log("exam-fab shown above exam window");

    Ok(())
}

/// 打开退出二次确认窗（async 同上，避免主线程死锁）
pub(crate) async fn open_exit_confirm(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("exit-confirm") {
        // 复用已创建的窗口。取消时只是 hide，必须显式 show 才会重新显示；
        // 早期版本只调 set_focus 就 return，隐藏态窗口不会重现，表现为「点了没反应」。
        let _ = w.show();
        let _ = w.set_focus();
        // 关键：确定按钮点击后会置 disabled=true，若上次退出成功则窗口被 hide，
        // 按钮保持禁用。复用同一 webview 时若不重置，第二次点确定完全没反应。
        // 复现日志特征：只有 "exit confirm shown (reuse)"，没有 "confirm exit start"。
        let _ = w.eval("var b=document.getElementById('ok'); if(b){b.disabled=false;}");
        exam_log("exit confirm shown (reuse), ok button reset");
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
            let _ = w.eval("var b=document.getElementById('ok'); if(b){b.disabled=false;}");
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
            exam_log(&format!("clear storage[{label}]: begin eval"));
            let _ = w.eval("try{localStorage.clear();sessionStorage.clear();}catch(e){}");
            exam_log(&format!("clear storage[{label}]: eval done"));

            // macOS：不要调用 clear_all_browsing_data()。
            // 它底层是 WKWebsiteDataStore.default().removeDataOfTypes，作用于
            // 全局共享 data store，会连带清掉引导页 main 窗口的数据，导致引导页
            // 异常；且复用 WKWebView 后该调用会长时间阻塞，让退出命令卡死。
            // 白屏根因已确认是 ATS 拦截 HTTP，与这里无关，故 macOS 只做 eval 清理。
            #[cfg(not(target_os = "macos"))]
            {
                exam_log(&format!("clear storage[{label}]: requesting browsing data clear"));
                if let Err(e) = w.clear_all_browsing_data() {
                    exam_log(&format!("clear browsing data({label}) failed: {e}"));
                } else {
                    exam_log(&format!("clear browsing data({label}) requested"));
                }
            }
        }
    }
    exam_log("clear storage: all done");
}

/// 在独立线程清考试数据，绝不阻塞退出命令。
///
/// macOS 复用 WKWebView 后，clear_all_browsing_data() 会阻塞很久（甚至不返回），
/// 若放在 confirm_exit_exam 的同步路径上，会出现「点确定退出没反应」。
/// 清数据与「回到引导页」没有先后依赖，放到后台异步执行即可。
fn spawn_clear_exam_web_storage(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        exam_log("background clear storage: thread started");
        clear_exam_web_storage(&app);
        exam_log("background clear storage: thread finished");
    });
}

/// 确认退出：解除锁定、清空考试页本地存储、收起考试相关窗口，回到引导页
#[tauri::command]
async fn confirm_exit_exam(app: tauri::AppHandle) {
    exam_log("confirm exit start");
    if let Some(exam) = app.get_webview_window("exam") {
        lockdown::deactivate(&exam);
    }
    exam_log("confirm exit: lockdown deactivated");

    // Windows 的悬浮按钮是原生分层窗口（不在 Tauri 窗口表里），必须显式销毁，
    // 否则退出考试后按钮会残留在屏幕上。
    #[cfg(target_os = "windows")]
    fab_win::destroy();

    // 清浏览数据放到独立线程，绝不阻塞退出命令。
    // macOS 复用 WKWebView 后 clear_all_browsing_data() 会阻塞，放在同步路径上
    // 会导致窗口收起与引导页显示永远执行不到（「点确定退出没反应」）。
    spawn_clear_exam_web_storage(app.clone());

    // 立即收起考试相关窗口，让用户马上回到引导页。
    let labels: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|label| {
            label.starts_with("block-")
                || matches!(
                    label.as_str(),
                    "exam" | "exam-fab" | "exam-backdrop" | "exit-confirm"
                )
        })
        .cloned()
        .collect();
    for label in labels {
        // exit-confirm 是发起本次 invoke 的窗口：立刻 hide/close 它会让 webview
        // 在执行回调时被销毁，命令可能收不到返回、前端按钮卡在禁用态。
        // 放到引导页显示之后再处理。
        if label == "exit-confirm" {
            continue;
        }
        if let Some(w) = app.get_webview_window(&label) {
            // macOS 保留并隐藏 WKWebView，下次进入直接复用；Windows 保持销毁语义。
            #[cfg(target_os = "macos")]
            let _ = w.hide();
            #[cfg(not(target_os = "macos"))]
            let _ = w.close();
            exam_log(&format!("confirm exit: window {label} hidden"));
        }
    }

    // 延迟显示引导页：deactivate 的 presentationOptions 清除是 run_on_main_thread
    // 异步执行的，立刻 show 可能与 kiosk 选项解除竞争，导致引导页显示了又消失。
    let guide = app.get_webview_window("main");
    let confirm_window = app.get_webview_window("exit-confirm");
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(guide) = guide {
            let _ = guide.show();
            let _ = guide.set_focus();
        }
        exam_log("confirm exit: guide shown after kiosk cleared");
        // 引导页已经在前面，此时再收起确认框，不影响 invoke 回调。
        if let Some(w) = confirm_window {
            #[cfg(target_os = "macos")]
            let _ = w.hide();
            #[cfg(not(target_os = "macos"))]
            let _ = w.close();
            exam_log("confirm exit: exit-confirm hidden after guide shown");
        }
    });
    exam_log("confirm exit done (guide show scheduled)");
}

/// 引导页「退出」：关闭客户端
#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// 前端读取当前运行环境（dev/test/prod），用于展示环境标识
#[tauri::command]
fn get_app_env() -> String {
    lockdown::APP_ENV.to_string()
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
        exam_log("ADMIN EXIT: password accepted, app.exit(0) - full shutdown");
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
