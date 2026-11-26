//! Windows 原生分层窗口实现的悬浮退出按钮。
//!
//! 为什么不用 WebView2 窗口：
//! 1. WebView2 有最小窗口约束，48px 窗口会被撑成横条，把圆按钮压成椭圆；
//! 2. Tauri 在 Windows 的透明窗口用软件 surface 填充底色且丢弃 alpha，
//!    窗口矩形必然露出半透明底色块。
//! 这里用 32bpp DIB + UpdateLayeredWindow 逐像素 alpha，画一个和窗口
//! 等大的抗锯齿圆按钮，窗口不存在多余区域，视觉上只有按钮。
//! 该模块仅在 Windows 编译，Mac 路径完全不受影响。

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, LoadCursorW, PostMessageW,
    PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, UpdateLayeredWindow, CS_HREDRAW,
    CS_VREDRAW, IDC_HAND, MSG, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WNDCLASSW,
};

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static FAB_HWND: AtomicIsize = AtomicIsize::new(0);

/// 创建原生悬浮按钮。x/y/side 均为物理像素。
pub fn create(app: tauri::AppHandle, x: i32, y: i32, side: i32) {
    let _ = APP.set(app);
    // 若上一次的按钮窗口还在（例如退出后再次进入考试），先销毁并等它真正退出，
    // 否则屏幕上会同时出现两个按钮。
    destroy();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
    while FAB_HWND.load(Ordering::SeqCst) != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    FAB_HWND.store(0, Ordering::SeqCst);
    std::thread::spawn(move || unsafe {
        run(x, y, side);
    });
}

/// 销毁原生悬浮按钮窗口。
///
/// 不能直接调 DestroyWindow：该窗口属于自建的消息循环线程，DestroyWindow 只能由
/// 创建线程调用。这里 PostMessage(WM_CLOSE)，由创建线程的 GetMessage 取出后经
/// DefWindowProc → DestroyWindow → WM_DESTROY（清句柄 + PostQuitMessage）→ 线程退出。
pub fn destroy() {
    let h = FAB_HWND.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    unsafe {
        PostMessageW(h as HWND, WM_CLOSE, 0, 0);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            if let Some(app) = APP.get() {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    crate::open_exit_confirm(app).await;
                });
            }
            0
        }
        WM_DESTROY => {
            FAB_HWND.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn run(x: i32, y: i32, side: i32) {
    let class_name: Vec<u16> = "KioskExitFab\0".encode_utf16().collect();
    let title: Vec<u16> = "退出考试\0".encode_utf16().collect();

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: GetModuleHandleW(std::ptr::null()),
        hIcon: std::ptr::null_mut(),
        hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hbrBackground: std::ptr::null_mut(),
    };
    RegisterClassW(&wc);

    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_POPUP,
        x,
        y,
        side,
        side,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        GetModuleHandleW(std::ptr::null()),
        std::ptr::null(),
    );
    if hwnd.is_null() {
        crate::exam_log("native fab: CreateWindowExW failed");
        return;
    }
    FAB_HWND.store(hwnd as isize, Ordering::SeqCst);

    paint(hwnd, x, y, side);
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    crate::exam_log(&format!("native fab ready pos=({x},{y}) side={side}"));

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 逐像素绘制抗锯齿圆按钮 + 白色退出图标，并作为分层窗口内容提交。
unsafe fn paint(hwnd: HWND, x: i32, y: i32, side: i32) {
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = side;
    bi.bmiHeader.biHeight = -side; // top-down DIB
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = 0; // BI_RGB

    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(
        std::ptr::null_mut(),
        &bi,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if bmp.is_null() || bits.is_null() {
        crate::exam_log("native fab: CreateDIBSection failed");
        return;
    }
    let memdc = CreateCompatibleDC(std::ptr::null_mut());
    let old = SelectObject(memdc, bmp);

    let n = (side * side * 4) as usize;
    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, n);
    draw_fab_pixels(buf, side);

    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // ptdst 是窗口在屏幕上的位置，会覆盖 CreateWindowEx 的坐标，必须传真实位置。
    let ptdst = POINT { x, y };
    let ptzero = POINT { x: 0, y: 0 };
    let psize = SIZE {
        cx: side,
        cy: side,
    };
    UpdateLayeredWindow(
        hwnd,
        std::ptr::null_mut(),
        &ptdst,
        &psize,
        memdc,
        &ptzero,
        0,
        &blend,
        ULW_ALPHA,
    );

    SelectObject(memdc, old);
    DeleteObject(bmp);
    DeleteDC(memdc);
}

/// 退出图标线段（24x24 viewBox 坐标，直角近似原 SVG 路径）。
const ICON_SEGMENTS: [([f32; 2], [f32; 2]); 6] = [
    ([9.0, 4.0], [5.0, 4.0]),    // 门框上
    ([5.0, 4.0], [5.0, 20.0]),   // 门框左
    ([5.0, 20.0], [9.0, 20.0]),  // 门框下
    ([9.0, 12.0], [19.0, 12.0]), // 箭头杆
    ([15.0, 8.0], [19.0, 12.0]), // 箭头尖上
    ([19.0, 12.0], [15.0, 16.0]),// 箭头尖下
];

fn dist_to_segment(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        ((px - a[0]) * dx + (py - a[1]) * dy) / l2
    }
    .clamp(0.0, 1.0);
    let qx = a[0] + t * dx;
    let qy = a[1] + t * dy;
    ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
}

fn clamp01(v: f32) -> f32 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// 填充 BGRA（premultiplied alpha）。圆边与图标边均用距离场做抗锯齿。
fn draw_fab_pixels(buf: &mut [u8], side: i32) {
    let s = side as f32;
    let cx = s / 2.0 - 0.5;
    let cy = cx;
    let radius = s / 2.0 - 0.6;
    // 按钮主色 #169e8c
    let (br, bg, bb): (f32, f32, f32) = (0x16 as f32, 0x9e as f32, 0x8c as f32);

    let icon_half = 0.9_f32; // stroke-width 1.8 的一半
    let icon_aa = 0.7_f32;
    // 图标区域：按钮中间 50%，即 24 单位映射到 [s/4, 3s/4]
    let icon_scale = 48.0_f32 / s;

    for y in 0..side {
        for x in 0..side {
            let idx = ((y * side + x) * 4) as usize;
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();

            // 圆的抗锯齿 alpha：半径内外各 0.5px 过渡
            let circle_a = clamp01(radius + 0.5 - dist);
            if circle_a <= 0.0 {
                buf[idx] = 0;
                buf[idx + 1] = 0;
                buf[idx + 2] = 0;
                buf[idx + 3] = 0;
                continue;
            }

            // 映射到图标坐标
            let ix = (x as f32 - s / 4.0) * icon_scale;
            let iy = (y as f32 - s / 4.0) * icon_scale;
            let mut min_d = f32::MAX;
            for (a, b) in ICON_SEGMENTS.iter() {
                let d = dist_to_segment(ix, iy, *a, *b);
                if d < min_d {
                    min_d = d;
                }
            }
            let icon_a = clamp01(icon_half + icon_aa - min_d);

            // 圆内底色与白色图标混合
            let r = br + (255.0 - br) * icon_a;
            let g = bg + (255.0 - bg) * icon_a;
            let b = bb + (255.0 - bb) * icon_a;

            // premultiply（AC_SRC_ALPHA 要求）
            let a8 = (circle_a * 255.0).round() as u8;
            buf[idx] = ((b * circle_a).round() as u8).min(a8);
            buf[idx + 1] = ((g * circle_a).round() as u8).min(a8);
            buf[idx + 2] = ((r * circle_a).round() as u8).min(a8);
            buf[idx + 3] = a8;
        }
    }
}

/// 供守护线程兜底：返回原生窗口句柄（若存在）。
#[allow(dead_code)]
pub fn hwnd() -> Option<HWND> {
    let h = FAB_HWND.load(Ordering::SeqCst);
    if h == 0 {
        None
    } else {
        Some(h as HWND)
    }
}
