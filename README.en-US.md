# herdr-shadow-pane

<p align="center"><img src="assets/logo.svg" width="180" alt="herdr-shadow-pane logo"></p>

English | [简体中文](README.md)

Shadow Pane: control multiple panes simultaneously.

![Example](examples/shadow-pane-example.png)


## Usage

Bind a shortcut (`prefix` defaults to `ctrl+b`) by adding the following to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+o"
type = "plugin_action"
command = "shaozk.herdr-shadow-pane.sync"
description = "herdr-shadow-pane: broadcast keystrokes"
```

Run `herdr server reload-config` after editing for the change to take effect. You can also trigger it from the herdr action menu.

Press `ctrl+q` to exit.


## Installation

Requires `herdr ≥ 0.8.0`.

```sh
herdr plugin install shaozk/herdr-shadow-pane
```

Local development (this repository is the plugin root):

```sh
make build      # cargo build --release and copy to bin/
make install    # herdr plugin link .
```
