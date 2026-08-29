# sync-panes

tmux 式 `synchronize-panes`，以 herdr 插件形式实现：把你的击键逐字符实时广播到当前 tab 中选定的多个窗格。

## 工作方式

0. 已绑定快捷键 `prefix+shift+o`（见下方"快捷键"），或从 herdr action 菜单触发 **Sync Panes: broadcast keystrokes**（或 `herdr plugin action invoke shaozk.sync-panes.sync`）。
1. action 只是一个薄启动器：调用 `herdr plugin pane open` 把控制台作为 overlay 窗格打开。
2. 控制台 TUI 首屏列出当前 tab 的全部窗格（自身窗格除外），空格勾选、回车进入广播页。
3. 广播页里你的每次击键都会实时转发给所有目标窗格；下方分格实时镜像每个目标的可见输出。
4. `Ctrl+Q` 退出，herdr 自动回收 overlay 窗格。

## 快捷键

在 `~/.config/herdr/config.toml` 中加入（`prefix` 默认为 `ctrl+b`）：

```toml
[[keys.command]]
key = "prefix+shift+o"
type = "plugin_action"
command = "shaozk.sync-panes.sync"
description = "sync-panes: broadcast keystrokes"
```

改完执行 `herdr server reload-config` 或按 `prefix+shift+r` 生效。

## 键位

| 选择页 | 行为 |
| --- | --- |
| `↑/↓` / `j/k` | 移动光标 |
| `space` | 勾选/取消 |
| `a` | 全选/全不选 |
| `enter` | 开始广播 |
| `esc` / `q` / `ctrl+q` | 退出 |

| 广播页 | 行为 |
| --- | --- |
| 可打印字符 | 原样广播 |
| `enter` / `backspace` / `esc` / `ctrl+c` | 以逻辑键广播（send-keys） |
| `ctrl+q` | 退出（`ctrl+c` 是广播内容，不是退出） |
| 其他控制键 | 忽略并提示（v1 最小键集） |

## 实现说明

- Rust + crossterm + ratatui，无运行时依赖。
- 输入通道：可打印字符走 `herdr pane send-text`，控制键走 `herdr pane send-keys`；发送并发扇出到所有存活目标。
- 输出镜像：每 500ms 对每个目标轮询 `herdr pane read --source visible --lines 14`。
- 目标窗格中途关闭会被标记 `✕ dead` 并停发；全部失效时自动退出。
- 广播语义为逐字符直通，不做 bracketed paste；向 agent TUI 广播时其界面对渐进输入的反应（如补全跳动）属预期行为。

## 开发与安装

本地开发（本仓库即插件根目录）：

```sh
sh scripts/build.sh      # cargo build --release 并拷贝到 bin/
herdr plugin link .
```

卸载：`herdr plugin unlink shaozk.sync-panes`。

调试：在任意窗格设置 `SYNC_PANES_DEBUG_LIST=1` 后运行 `bin/sync-panes`，打印解析到的 tab 上下文与窗格列表后退出。

## Roadmap（v1 明确不做）

- workspace / 跨 tab 作用域
- 逐行（Enter 提交）广播模式
- bracketed paste 支持
- 全量键位映射（方向键、Tab、Home/End、PgUp/PgDn、Ctrl+字母全表、F1–F12）
- 目标选择持久化
