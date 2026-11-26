fn main() {
    // 分环境打包通过编译期环境变量切换（见 lockdown/mod.rs）。
    // cargo 默认不追踪环境变量变化，必须显式声明，否则切换环境后
    // 增量编译会复用旧值，打出错误环境的包。
    println!("cargo:rerun-if-env-changed=KIOSK_ENV");
    println!("cargo:rerun-if-env-changed=KIOSK_EXAM_URL");
    println!("cargo:rerun-if-env-changed=KIOSK_UPDATER");
    tauri_build::build()
}
