# Kiosk 客户端打包与发布步骤

## 一、一次性准备（已完成，仅备查）

1. **更新签名密钥**：`.keys/kiosk.key`（私钥，已 gitignore，**不要提交、不要泄露**）+ `.keys/kiosk.key.pub`（公钥）。
   - 重新生成：`cargo tauri signer generate -w .keys/kiosk.key --ci`
2. **公钥已写入配置**：`src-tauri/tauri.conf.json` 及各环境配置的 `plugins.updater.pubkey` 必须与 `kiosk.key.pub` 一致（当前一致）。
3. **服务器目录约定**（更新文件按环境分目录）：
   - dev：`https://updates.elecbalance.com/kiosk-dev/`
   - test：`https://updates.elecbalance.com/kiosk-test/`
   - prod：`https://updates.elecbalance.com/kiosk/`

## 二、每次发布流程（Windows 本机）

### 1. 改版本号

编辑 `src-tauri/tauri.conf.json` 的 `version` 字段（如 `0.1.0` → `0.2.0`）。客户端靠这个字段与服务器 `latest.json` 的 `version` 比较，**必须改**才触发更新。

### 2. 打包

```bat
scripts\build.cmd prod        :: 生产包
scripts\build.cmd test        :: 测试包
scripts\build.cmd dev         :: 开发包
scripts\build.cmd prod --debug  :: 调试构建（快速验证用，不要发布）
```

脚本会自动设置 `KIOSK_ENV`、注入签名私钥并合并对应环境的 `tauri.<env>.conf.json`。

### 3. 确认产物

位于 `src-tauri/target/release/bundle/nsis/`：

- `Elecbalance Kiosk_0.2.0_x64-setup.exe` —— 安装包（发給用户装）
- `Elecbalance Kiosk_0.2.0_x64-setup.exe.sig` —— 更新签名（内容贴进 latest.json）

> 环境隔离检查点：test/dev 包名是 `Elecbalance Kiosk Test/Dev_...`，装之前核对名字，**不要发错环境的包**。

### 4. 上传更新服务器

把 **setup.exe** 上传到对应环境目录，然后在同目录放/更新 `latest.json`：

```json
{
  "version": "0.2.0",
  "notes": "本次更新说明",
  "pub_date": "2026-11-26T12:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<.sig 文件的完整内容，原样粘贴>",
      "url": "https://updates.elecbalance.com/kiosk/Elecbalance%20Kiosk_0.2.0_x64-setup.exe"
    }
  }
}
```

要点：
- `signature` = `.sig` 文件全文（一行 base64，含开头 `dW50cnVzdGVkIGNvbW1lbnQ6`），不是公钥。
- `url` 必须能直接下载到 exe，空格用 `%20` 或改用不带空格的文件名。
- Mac 端发布时在 `platforms` 里追加 `darwin-aarch64` / `darwin-x86_64` 条目（`.app.tar.gz` + 对应 `.sig`），与 Windows 条目共存于同一文件。

### 5. 验证自动更新

1. 机器上保留一个**旧版本**客户端（版本号 < latest.json 的 version）。
2. 正常启动客户端，观察后台更新（失败会静默跳过，不影响使用）。
3. 看到应用自动重启且版本变为新版即成功。
4. 排查：更新日志走 stderr；进入考试的诊断日志在 `%TEMP%\elecbalance-exam.log`。

## 三、常见问题

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 客户端不更新 | latest.json 的 version ≤ 本地版本 | 递增版本号重新发布 |
| 更新失败被跳过 | 服务器不可达 / url 404 / 签名不匹配 | 检查 latest.json 的 url 可直接下载、signature 与私钥匹配 |
| 打包报签名错误 | `.keys/kiosk.key` 不存在 | `cargo tauri signer generate -w .keys/kiosk.key --ci` 并同步 pubkey 到配置 |
| 装错环境 | 忘了传环境参数 | 始终用 `scripts\build.cmd dev/test/prod`，别裸跑 `cargo tauri build` |

## 四、Mac 端（暂未真机验证）

```bash
scripts/build.sh prod   # 用法与 Windows 相同
```

产物在 `src-tauri/target/release/bundle/macos/*.app.tar.gz`（+ `.sig`）和 `dmg/`。发布时把 darwin 条目加进对应环境的 latest.json；注意 Mac 更新要求应用装在 `/Applications`。
