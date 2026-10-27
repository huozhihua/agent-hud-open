# Cursor 额度钥匙串回退与会话 Token（本地补丁）

| 版本 | 日期 | 状态 |
| --- | --- | --- |
| 1.1.0 | 2026-10-27 | 现行 |

## 背景

1. 较新的 Cursor 会把 `state.vscdb` 里的 `cursorAuth/accessToken` 写成 `cursor-safe-storage:v1:…` 密文，额度读取失败。
2. Cursor 会话列表来自数据库，但 **会话 Token 只来自 `stop` completion hook**；预览入口未委托固定可执行文件时不会安装 `~/.cursor/hooks.json`，Token 恒为 0。

明文 JWT 仍在 macOS 钥匙串：`service=cursor-access-token`。

## 改动

### 额度（钥匙串回退）

`src/crates/agents/src/cursor/account.rs`：

1. 数据库里若是三段 JWT，行为与原来相同。
2. 否则读取钥匙串 `cursor-access-token`，再请求 `https://cursor.com/api/usage-summary`。

### 会话 Token（hook 委托）

`src/apps/desktop/src-tauri/src/main.rs` + `src/crates/runtime/src/lib.rs`：

1. 从已安装的 `*.app/Contents/MacOS/*` 启动时，将当前二进制委托为 `canonical_executable`。
2. `target`、磁盘映像、`AppTranslocation` 路径不委托（避免开发构建改写客户端配置）。
3. 若设置里 `clientHooks` 为开，启动时安装 Cursor 的 `stop` → `--completion-hook cursor`。

**不影响 Cursor 计费**：hook 在生成结束后只读 payload 中的用量数字。

## 使用要求

- 将 App 装到 `/Applications`（或其它稳定 `.app` 路径），不要长期从 dmg 卷内直接跑。
- 设置 → 打开「客户端回调」；Cursor ≥ 3.27。
- 去隔离标记：`xattr -dr com.apple.quarantine "/Applications/Agent HUD Next.app"`
- 历史会话无 Token；安装 hook 之后的新会话才会有。

## 私有仓库

- 仓库：https://github.com/huozhihua/agent-hud-open （private）
- `origin` → 你的私有仓；`upstream` → `jazzenchen/agent-hud-open`

## 构建产物

```text
dist/Agent HUD Next.app
dist/Agent HUD Next_0.1.0_aarch64.dmg
```

Bundle ID：`com.agenthud.next.preview`（可与正式版 `app.agenthud.mac` 并存）。

## 验证

- `cargo test -p agent-hud-agents --lib cursor::account`：含加密回退用例
- 本机钥匙串 JWT + Cookie 调 `usage-summary` 返回 200 / Pro
- 从 Applications 启动后 `~/.cursor/hooks.json` 含 `--completion-hook cursor`
- 新 Cursor 会话结束后 HUD Token > 0
