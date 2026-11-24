# Elecbalance Kiosk 客户端

用 Rust (Tauri v2) 内嵌指定网址的跨平台霸屏客户端：启动即全屏锁定，屏蔽切屏快捷键，检测并处置后台运行的远程控制 / 通讯软件。

- **内嵌网址**：`https://sim.elecbalance.com/login?queryCode=1794879926545`（修改见 `src-tauri/tauri.conf.json` → `app.windows[0].url`）

## 目录结构

```
kiosk-client/
├── dist/index.html                  # frontendDist 占位页
└── src-tauri/
    ├── tauri.conf.json              # 窗口/打包配置（URL 在这里改）
    ├── Cargo.toml
    ├── build.rs
    ├── icons/icon.ico
    └── src/
        ├── main.rs                  # 应用入口：单实例 + setup 进入锁定模式
        └── lockdown/
            ├── mod.rs               # 跨平台入口 + 进程黑名单特征库
            ├── windows.rs           # 键盘钩子 / 焦点守护 / 进程监控
            └── macos.rs             # macOS 基础版 + 原生 Kiosk 增强说明
```

## 构建与运行

### 本机开发调试（Windows，需先装 WebView2 Runtime，Win10/11 自带）

```bash
cd kiosk-client/src-tauri
cargo run            # debug 模式，带控制台日志，方便排查
```

### 打生产包（Windows exe / NSIS 安装包）

```bash
cd kiosk-client
cargo install tauri-cli --version "^2"   # 首次需要
cargo tauri build
# 产物在 src-tauri/target/release/ 与 src-tauri/target/release/bundle/
```

### macOS 构建

macOS 包只能在 Mac 系统上产出（Apple SDK 限制，Windows 无法交叉编译）；同理 Windows 的
NSIS 包也无法在 macOS 上产出。两端的云构建配置均已内置：

- **`.github/workflows/build-macos.yml`**：Actions 页手动运行（或推 `v*` tag）→
  Artifacts 下载 `.dmg` / `.app`
- **`.github/workflows/build-windows.yml`**：同上 → Artifacts 下载 NSIS 安装包 `.exe`

私有仓库有免费云构建分钟额度，公开仓库不限。

- **本机构建**：在对应系统的机器上：

```bash
# macOS（Mac 机器）
cd kiosk-client/src-tauri
cargo tauri build --bundles dmg

# Windows（Windows 机器）
cd kiosk-client
cargo tauri build
```

原生 Kiosk 增强（hideDock / disableProcessSwitching / disableForceQuit 等）参考
`src/lockdown/macos.rs` 顶部注释，用 `objc2-app-kit` 设置
`NSApplicationPresentationOptions` 即可；分发前需代码签名 + 公证。

## 锁定能力清单

| 能力 | 实现 | 状态 |
|---|---|---|
| **启动安全门禁** | 窗口隐藏 → 检测分屏（多显示器）+ 远控软件 → 不通过弹窗「重试检测 / 退出系统」，通过后才显示窗口进入霸屏 | 双平台 |
| 全屏霸屏 | `set_fullscreen(true)` + 无边框 | Windows / macOS |
| 窗口置顶 | 500ms 焦点守护 + `SetWindowPos(HWND_TOPMOST)` | Windows |
| 焦点夺回 | `GetForegroundWindow` 校验 + Alt 辅助 `SetForegroundWindow` | Windows |
| 屏蔽 Alt+Tab / Win键 / Ctrl+Shift+Esc / Alt+F4 / Alt+Space / F11 | `SetWindowsHookExW(WH_KEYBOARD_LL)` | Windows |
| 远控软件检测（ToDesk/向日葵/TeamViewer/AnyDesk/RustDesk 等） | sysinfo 2s 轮询，命中自动终止（可配） | 双平台 |
| 通讯软件检测（微信/QQ/钉钉/飞书等） | 默认仅告警不终止（可配） | 双平台 |
| 启动预检 | 启动时全量扫描并记录日志 | 双平台 |
| 单实例 | tauri-plugin-single-instance | 双平台 |
| 日志 | `%TEMP%/elecbalance-kiosk.log` | Windows |
| 日志 | `/tmp/elecbalance-kiosk.log` | macOS |
| **管理员退出** | 全局热键 `Ctrl+Alt+Shift+Q` → 置顶口令窗 → 口令正确解锁退出，错误拒绝并记录（错误响应延迟 1s 防暴力尝试） | 双平台 |

## 关键配置（`src-tauri/src/lockdown/mod.rs`）

```rust
pub const AUTO_KILL_REMOTE: bool = true;         // 远控软件：检测到即终止
pub const AUTO_KILL_COMMUNICATION: bool = false; // 通讯软件：仅告警，按需改 true
pub const ADMIN_EXIT_HOTKEY: &str = "ctrl+alt+shift+q"; // 管理员退出热键
pub const ADMIN_PASSWORD: &str = "elecbalance2026";     // 管理员退出口令，修改后重新打包生效
```

黑名单 `REMOTE_CONTROL_APPS` / `COMMUNICATION_APPS` 同文件维护，
生产环境建议改为云端特征库热更新。

## 管理员退出

霸屏状态下按 **`Ctrl+Alt+Shift+Q`**（Windows / macOS 通用，macOS 上为
Control+Option+Shift+Q），弹出置顶口令输入窗：

- 口令正确 → 立即解锁退出（键盘钩子、置顶、Kiosk 选项随进程退出自动失效）
- 口令错误 → 提示「口令错误」，焦点拉回输入框，记录日志；每次错误延迟 1 秒防暴力尝试
- 取消 / 关闭窗口 → 焦点自动还给霸屏主窗口，锁定不受影响
- 口令窗打开期间，焦点守护不会抢占本进程窗口（输入正常）

口令与热键在 `src-tauri/src/lockdown/mod.rs` 中修改，改后重新打包生效。
注意：口令编译进二进制，属「防普通用户」强度；需要更强管控可改为云端校验口令。

## 已知边界（系统级安全底线，无法用客户端绕过）

- Windows `Ctrl+Alt+Del` 由内核 SAS 处理，用户态程序不可拦截（可用组策略
  `DisableTaskMgr` 缓解）；需要更强管控用 Shell 替换 / Assigned Access 部署。
- macOS 用户可重启进恢复模式；更强制走 MDM 单应用模式。
- 发布前：Windows 建议代码签名；macOS 必须签名 + 公证。

## 自动更新（tauri-plugin-updater）

启动流程：隐藏窗口 → **检查更新（有新版则下载安装并自动重启）** → 安全门禁 → 全屏霸屏。
未配置密钥/更新服务器时自动跳过（日志提示），不影响正常启动。

> **当前状态：关闭签名产物**（`createUpdaterArtifacts: false`，内部使用免签名直接打包）。
> 代码层面的更新逻辑已就绪，日后要启用时按下方步骤操作即可。

### 内部使用（无签名，当前模式）

- **Windows**：直接 `cargo tauri build` 出 NSIS 安装包。因未做代码签名，
  首次运行可能触发 SmartScreen 提示，点「更多信息 → 仍要运行」即可；
  也可右键 exe → 属性 → 勾选「解除锁定」。
- **macOS**：在 Mac 上 `cargo tauri build --bundles dmg`。未签名包首次打开会被
  Gatekeeper 拦，处理方式任选：
  - 右键 App →「打开」（弹窗中再点打开）
  - 或终端去掉隔离标记：`xattr -d com.apple.quarantine "/Applications/Elecbalance Kiosk.app"`
  - 或临时自签：`codesign --force --deep -s - "Elecbalance Kiosk.app"`
- **更新分发**：内部场景最简单的做法是把新版安装包放到内网文件服务器/网盘，
  通知用户自行覆盖安装；正式启用自动更新见下节。

### 启用自动更新（需要签名时再开）

1. **生成签名密钥对**（一次性）：

```bash
cargo tauri signer generate -w ~/.tauri/kiosk.key
```

2. **写入公钥**：把生成的公钥填入 `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`，
   并把 `endpoints` 改为你的更新服务器地址；同时把
   `bundle.createUpdaterArtifacts` 改回 `true`。

3. **打包时带私钥**（用于签名更新产物）：

```bash
export TAURI_SIGNING_PRIVATE_KEY=~/.tauri/kiosk.key          # Windows: set TAURI_SIGNING_PRIVATE_KEY=...
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=你的密码            # 无密码可留空
cargo tauri build
```

构建会自动生成更新产物：`*.exe`（NSIS 安装包）、`*.exe.sig` 签名、`latest.json` 版本清单，
位于 `src-tauri/target/release/bundle/`。

4. **发布**：把安装包、`.sig`、`latest.json` 上传到更新服务器同一目录即可。
   客户端启动时拉取 `latest.json`，比对版本号 → 验签 → 下载安装 → 自动重启。

### 注意

- macOS 端更新产物为 `.app.tar.gz` + `.sig`，同样由 `cargo tauri build` 自动生成
- 打正式安装包前记得把 `"createUpdaterArtifacts": true` 保持开启；日常本地调试不签名时
  用 `cargo build` 不受影响
- 私钥丢失则旧版本客户端无法验证新版本更新包，务必备份

## 后续增强路线

见《rust-kiosk-client-技术路线.md》：WMI 进程事件订阅（毫秒级）、数字签名校验、
云端特征库、违规上报、Windows Shell 替换部署包。
