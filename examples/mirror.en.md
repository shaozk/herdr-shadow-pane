# Mirror output example

When the broadcast page opens, it lays out every other pane of the current tab as side-by-side mirrors. Each mirror shows:

- The target pane's visible output (with original terminal styling — fg/bg, bold, reverse, etc.)
- A shadow cursor — one reversed-space cell, shown as `█` here — right after the last non-empty cell of the last line. That's where the next broadcast character will land.
- A 1-column dim separator between mirrors; no borders.

Example with two targets (helix + opencode):

```
 hello.txt  12 lines  [NORMAL]█   │  > fix the bug in line 42...█
```

The shadow cursor advances as you type; pressing Enter fans out each target's output into its mirror; a closed target shows `✕ dead` in red; when all targets die the broadcast page auto-exits.
