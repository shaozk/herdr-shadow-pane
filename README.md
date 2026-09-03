# herdr-shadow-pane

[English](README.en-US.md) | 简体中文

影分身面板：同时控制多个面板。

![示例](examples/shadow-pane-example.png)


## 使用

绑定快捷键（`prefix` 默认 `ctrl+b`），在 `~/.config/herdr/config.toml` 加入：

```toml
[[keys.command]]
key = "prefix+shift+o"
type = "plugin_action"
command = "shaozk.herdr-shadow-pane.sync"
description = "herdr-shadow-pane: broadcast keystrokes"
```

改完执行 `herdr server reload-config` 生效；也可从 herdr action 菜单触发。

使用 `ctrl+q` 退出。


## 安装

要求 `herdr ≥ 0.8.0`。

```sh
herdr plugin install shaozk/herdr-shadow-pane
```

本地开发（本仓库即插件根目录）：

```sh
make build      # cargo build --release 并拷贝到 bin/
make install    # herdr plugin link .
```
