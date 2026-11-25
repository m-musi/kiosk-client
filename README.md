# Elecbalance Kiosk 客户端

用 Rust (Tauri v2) 实现的跨平台考试客户端：启动显示引导页，环境检测通过后进入全屏霸屏考试，屏蔽切屏快捷键，检测并处置后台运行的远程控制 / 通讯软件，退出时回到引导页。

- **系统名称**：山西省电力仿真考试客户端
- **内嵌网址**：`https://sim.elecbalance.com/login`（修改见 `src-tauri/src/lockdown/mod.rs` → `EXAM_URL`）

## 目录结构

```
kiosk-client/
├── dist/
│   ├── guide.html                   # 引导页：考试须知 + 进入/退出 + 远控检测状态
│   ├── exit-fab.html                # 考试网页上层的悬浮退出按钮
│   ├── exam-exit-confirm.html       # 退出考试二次确认弹窗
│   ├── block.html                   # 副屏遮挡页：正在考试中，请在本机屏幕操作
│   ├── exit.html                    # 管理员口令窗
│   └── index.html                   # 占位页
└── src-tauri/
    ├── tauri.conf.json              # 引导页窗口 / 打包配置
    ├── Cargo.toml
    ├── build.rs
    ├── icons/icon.ico
    └── src/
        ├── main.rs                  # 应用入口 + 进入/退出考试命令 + 窗口编排
        └── lockdown/
            ├── mod.rs               # 锁定状态 / 远控检测 / 考试地址 / 黑名单
            ├── windows.rs           # 键盘钩子 / 焦点守护 / 手势禁用 / 进程监控
            └── macos.rs             # 原生 Kiosk 选项 / 前台守护 / 进程监控
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
| 引导页 | 启动显示「山西省电力仿真考试客户端」，列出考试注意事项，提供「进入考试 / 退出」 | 双平台 |
| 远控检测与进入控制 | 引导页 800ms 轮询，命中远程软件时禁用「进入考试」并提示「请关闭远程软件」；进入前复核 | 双平台 |
| 全屏霸屏 | `set_fullscreen(true)` + 无边框 | Windows / macOS |
| 窗口置顶 / 焦点夺回 | 500ms 焦点守护 + `SetWindowPos(HWND_TOPMOST)` + `GetForegroundWindow` 校验 | Windows |
| 屏蔽 Alt+Tab / Win键 / Ctrl+Shift+Esc / Alt+F4 / Alt+Space / F11 | `SetWindowsHookExW(WH_KEYBOARD_LL)`，仅在考试锁定状态下生效 | Windows |
| 副屏遮挡 | 取消多屏检测；进入考试后为每块非主屏创建整屏遮挡页「正在考试中，请在本机屏幕操作」 | 双平台 |
| 禁用三指/四指切换桌面 | 进入考试写入 `ThreeFingerSlideEnabled` / `FourFingerSlideEnabled = 0`，退出时恢复原值 | Windows 精确式触控板 |
| 远控软件检测（ToDesk/向日葵/TeamViewer/AnyDesk/RustDesk 等） | sysinfo 2s 轮询，命中自动终止（可配） | 双平台 |
| 通讯软件检测（微信/QQ/钉钉/飞书等） | 默认自动终止（可配） | 双平台 |
| 悬浮退出 | 考试网页上层独立置顶按钮 → 二次确认「成绩上传且不可更改」→ 解除锁定回到引导页 | 双平台 |
| 单实例 | tauri-plugin-single-instance | 双平台 |
| 日志 | `%TEMP%/elecbalance-kiosk.log` | Windows |
| 日志 | `/tmp/elecbalance-kiosk.log` | macOS |
| 管理员退出 | 全局热键 `Ctrl+Alt+Shift+Q` → 置顶口令窗 → 口令正确则关闭客户端（错误延迟 1s） | 双平台 |

## 关键配置（`src-tauri/src/lockdown/mod.rs`）

```rust
pub const EXAM_URL: &str = "https://sim.elecbalance.com/login"; // 内嵌考试网址
pub const AUTO_KILL_REMOTE: bool = true;         // 远控软件：检测到即终止
pub const AUTO_KILL_COMMUNICATION: bool = true;  // 通讯软件：默认终止，按需改 false
pub const ADMIN_EXIT_HOTKEY: &str = "ctrl+alt+shift+q"; // 管理员退出热键
pub const ADMIN_PASSWORD: &str = "elecbalance2026";     // 管理员退出口令，修改后重新打包生效
```

黑名单 `REMOTE_CONTROL_APPS` / `COMMUNICATION_APPS` 同文件维护，
生产环境建议改为云端特征库热更新。

## 管理员退出

霸屏状态下按 **`Ctrl+Alt+Shift+Q`**（Windows / macOS 通用，macOS 上为
Control+Option+Shift+Q），弹出置顶口令输入窗：

- 口令正确 → 立即关闭客户端（键盘钩子、置顶、Kiosk 选项随进程退出自动失效）
- 口令错误 → 提示「口令错误」，焦点拉回输入框，记录日志；每次错误延迟 1 秒防暴力尝试
- 取消 / 关闭窗口 → 焦点自动还给考试窗口，锁定不受影响
- 口令窗打开期间，焦点守护不会抢占本进程窗口（输入正常）

口令与热键在 `src-tauri/src/lockdown/mod.rs` 中修改，改后重新打包生效。
注意：口令编译进二进制，属「防普通用户」强度；需要更强管控可改为云端校验口令。

考生退出走另一条路径：点击考试页右下角悬浮按钮 → 二次确认
「退出考试后，当前操作成绩将上传后台且不可更改，您是否确定退出？」→ 解除锁定、
关闭考试/遮挡/悬浮窗，回到引导页；不需要管理员口令。

## 已知边界（系统级安全底线，无法用客户端绕过）

- Windows `Ctrl+Alt+Del` 由内核 SAS 处理，用户态程序不可拦截（可用组策略
  `DisableTaskMgr` 缓解）；需要更强管控用 Shell 替换 / Assigned Access 部署。
- 三指/四指手势禁用仅覆盖 Windows 精确式触控板的注册表开关；厂商私有驱动手势、
  macOS trackpad 手势不受影响，需通过系统设置或 MDM 管控。正常退出考试会恢复原设置，
  进程被强制结束时尚未恢复的值需手工还原。
- macOS 用户可重启进恢复模式；更强制走 MDM 单应用模式。
- 发布前：Windows 建议代码签名；macOS 必须签名 + 公证。

## 自动更新（tauri-plugin-updater）

启动流程：显示引导页 → **检查更新（有新版则下载安装并自动重启）** → 考生点「进入考试」
→ 远控复核 → 打开考试窗口 + 副屏遮挡 + 悬浮退出按钮 → 全屏霸屏。
未配置密钥/更新服务器时自动跳过（日志提示），不影响正常启动。

> **当前状态：关闭签名产物与更新检查**（`createUpdaterArtifacts: false`、
> `lockdown::UPDATER_ENABLED = false`）。此前在启动 `setup` 中同步检查更新，
> 更新服务器不可达时会导致窗口一直不显示；现已跳过检查，且检查逻辑改在后台线程执行，
> 不会再阻塞引导页。日后要启用时按下方步骤操作，并把 `UPDATER_ENABLED` 改为 `true`。

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
