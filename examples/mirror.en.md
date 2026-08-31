# Mirror output example

When the broadcast page opens, it lays out every other pane of the current tab as side-by-side mirrors. Each mirror shows:

- The target pane's visible output (with original terminal styling — fg/bg, bold, reverse, etc.)
- A shadow cursor (rendered as the cell's existing character with REVERSED applied — same look as a real terminal cursor; shown as `█` here) anchored at the next landing point of your broadcast input. The console tracks the row that just changed via screen diff, so the cursor follows your typing instead of being pinned to the status line.
- A 1-column dim separator between mirrors; no borders.

Example with two targets (helix + opencode):

```
 hello.txt  12 lines  [NORMAL]█   │  > fix the bug in line 42...█
```

The shadow cursor advances as you type; pressing Enter fans out each target's output into its mirror; a closed target shows `✕ dead` in red; when all targets die the broadcast page auto-exits.