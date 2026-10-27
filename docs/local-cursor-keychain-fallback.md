# Cursor 加密 token 钥匙串回退（本地补丁）

| 版本 | 日期 | 状态 |
| --- | --- | --- |
| 1.0.0 | 2026-10-27 | 现行 |

## 背景

较新的 Cursor 会把 `state.vscdb` 里的 `cursorAuth/accessToken` 写成 `cursor-safe-storage:v1:…` 密文。Agent HUD 原先只认明文 JWT，于是额度显示「暂不可用 / 请先登录」。

明文 JWT 仍在 macOS 钥匙串：`service=cursor-access-token`。

## 改动

`src/crates/agents/src/cursor/account.rs`：

1. 数据库里若是三段 JWT，行为与原来相同。
2. 否则读取钥匙串 `cursor-access-token`，再请求 `https://cursor.com/api/usage-summary`。

## 构建产物

```text
dist/Agent HUD Next.app
dist/Agent HUD Next_0.1.0_aarch64.dmg
```

Bundle ID：`com.agenthud.next.preview`（可与正式版 `app.agenthud.mac` 并存）。

## 验证

- `cargo test -p agent-hud-agents --lib cursor::account`：6 passed（含加密回退与无钥匙串失败用例）
- 本机钥匙串 JWT + Cookie 调 `usage-summary` 返回 200 / Pro
