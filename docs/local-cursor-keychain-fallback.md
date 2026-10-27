# Cursor 额度钥匙串回退、会话 Token 与授权提醒（本地补丁）

| 版本 | 日期 | 状态 |
| --- | --- | --- |
| 1.3.0 | 2026-10-27 | 现行 |

## 版本摘要

| 版本 | 日期 | 摘要 |
| --- | --- | --- |
| 1.3.0 | 2026-10-27 | Always 按仓落盘：Shell 命令前缀、文件路径、MCP `server:tool` |
| 1.2.0 | 2026-10-27 | Cursor gate hooks → HUD 授权卡；`failClosed` |
| 1.1.0 | 2026-10-27 | 钥匙串额度回退 + `stop` 会话 Token |

## 背景

1. 较新的 Cursor 会把 `state.vscdb` 里的 `cursorAuth/accessToken` 写成 `cursor-safe-storage:v1:…` 密文，额度读取失败。
2. Cursor 会话列表来自数据库，但 **会话 Token 只来自 `stop` completion hook**；预览入口未委托固定可执行文件时不会安装 `~/.cursor/hooks.json`，Token 恒为 0。子 agent 的 composer 往往没有自己的 `stop` 行（用量记在父会话的 `conversation_id` 上）。会话详情不把这显示成 0%，也不把父用量拆给子会话。
3. Cursor **没有** Claude 式 `PermissionRequest`；要在 Agent HUD Next 上点同意，需用 Cursor 原生 gate hooks 挂起等待 HUD。

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
3. 若设置里 `clientHooks` 为开，启动时安装 Cursor hooks。

**不影响 Cursor 计费**：completion hook 在生成结束后只读 payload 中的用量数字。

### 授权提醒（gate → HUD）

`src/crates/agents/src/cursor/hooks.rs` + `hooks/{configuration,request}.rs` + `platform/hooks/always.rs`：

1. `--permission-hook cursor` 安装到：`beforeShellExecution`、`beforeMCPExecution`、`beforeReadFile`、`preToolUse`（matcher 排除已覆盖工具）、`subagentStart`。
2. 每条 handler：`failClosed: true`，`timeout: 300`（秒）；保留原有 `stop` completion。
3. stdin 归一化为 HUD `Request`；stdout 回 `{"permission":"allow"|"deny"}`（不依赖 Cursor 的 `ask`）。
4. **Always** 写在 HUD 自己的按仓表里，跨会话有效，落盘到  
   `~/Library/Application Support/app.agenthud.next/hooks/workspace-always.json`。  
   **不写** Cursor 的 `permissions.json`、`terminalAllowlist` 或 IDE 命令允许列表。卡片文案是「在本工作区，总是允许将信任 …」，不是「写入 Cursor 允许列表」。
5. 允许一次不落盘。拒绝不落盘。Always 写盘失败时规则不进入内存，卡片留下并显示错误，下次启动不会凭空放行。
6. Cursor 取消授权（hook 进程被杀掉）时，Unix 回调套接字断开，服务端撤卡。stdin 读到 EOF 不是取消信号。

匹配键是规范化后的工作区路径（绝对路径、去掉末尾斜杠，存在时做 canonicalize）。多根窗口会看**每一个** `workspace_roots`，而不是只看 `[0]`：

| 载荷 | 写入哪条根 | 之后哪些请求命中 |
| --- | --- | --- |
| `cwd` 落在某个根里 | 包含 `cwd` 的最长根 | 该根仍出现在 `workspace_roots` 里，或请求没有根、只带同一个 `cwd` |
| `cwd` 不在任何根里，或没有 `cwd` | 列出的每一条根 | 请求的根列表与其中任一条相交 |
| 没有根，只有 `cwd` | 规范化后的 `cwd` | 同一个 `cwd` |

Shell 前缀（对齐 Cursor `terminalAllowlist` 的前缀精神，不把整个 Shell 收成 `*`）：

| 本次命令 | 写入的前缀 | 之后自动放行 | 不放行 |
| --- | --- | --- | --- |
| `git` | `git` | `git status`、`git diff` | `gitk` |
| `git status --short` | `git status` | `git status`、`git status -sb` | `git diff`、`git statusfoo` |
| `npm install express` | `npm install` | `npm install left-pad` | `npm test` |
| `ls -la` | `ls` | `ls /tmp` | `lsblk` |
| `git status && rm -rf /` | （不提供 Always） | — | 复合命令每次都要人点 |

前缀怎么截出来：只接受一条简单命令（引号外的 `&&`、`||`、`|`、`;`、`&`、重定向、换行、反引号、`$()` 都不提供 Always）。去掉开头的 `NAME=value`。保留可执行文件名原样（`git` 或 `/usr/bin/git`）。下一项若是子命令词（不是 `-` 开关，也不是路径或通配）就一并写入，例如 `git status`、`npm install`；再后面的参数不进前缀。大小写敏感，并停在 token 边界。磁盘上若已有 Cursor 写法 `npm:install*`，或 Claude 写法 `npm test:*`，按同样的前缀/通配比较。复合命令即使前缀已经存在也不会自动放行（比 Cursor 的纯字符串前缀更严，避免 `git status` 带过 `git status && rm`）。

其它工具：

| 工具 | Always 范围 |
| --- | --- |
| MCP | 仅该次 `server:tool`（例如 `github:create_issue`），不会放成 `server:*` 或全部 MCP |
| Read / Write / Edit / Delete | 该次文件的规范化路径；没有路径则不提供 Always |
| Grep / Glob | 仅当载荷带 `path`（或 `target_directory`）时按该路径；否则不提供 |
| WebFetch | 该次 URL 全文 |
| Task / 子代理 / WebSearch 等没有安全范围的调用 | 不提供 Always，避免静默的整工具 `*` |

**双闸门**：HUD 放行之后，Cursor 的沙箱、Auto-review 和外部文件保护仍然生效。hook `allow` 不是 Cursor 命令允许列表。`deny` 始终生效。

## 使用要求

- 将 App 装到 `/Applications`（或其它稳定 `.app` 路径），不要长期从 dmg 卷内直接跑。
- 设置 → 打开「客户端回调」；Cursor ≥ 3.27。
- 去隔离标记：`xattr -dr com.apple.quarantine "/Applications/Agent HUD Next.app"`
- 历史会话无 Token；安装 hook 之后的新会话才会有。子 agent 的 Token 今天不能从本地 `stop` hook 单独拿到，详情里写「用量未单独上报」，父会话合计保持原样。
- 授权：Cursor Agent 触发工具时 HUD 出卡。Shell 点 Always 只信任该命令前缀（例如 `git status`），不是所有 Shell。同一工作区里前缀命中的后续命令（含新对话、重启 Next）不再弹；别的前缀、别的路径、别的 MCP 工具、别的工作区仍弹。清信任可删 `workspace-always.json`。

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

- `cargo test -p agent-hud-agents --lib hooks::request`
- `cargo test -p agent-hud-platform --lib hooks::always`
- 从 Applications 启动后 `~/.cursor/hooks.json` 含 gate 事件与 `failClosed`
- 本仓对 `git status` 点 Always → 新对话里 `git status --short` 不弹，`git diff` 仍弹；另一工作区仍弹；重启 Next 后前缀仍在
- 本仓对某个文件 Edit 点 Always → 只该路径不再弹，同仓其它文件仍弹
- Cursor 取消授权（杀掉 hook 进程）时 HUD 卡撤掉
