## Каркас окна (D-02, №61)
ui-panel-count = { $count } { $count ->
    [one] item
   *[other] items
}
ui-status-selected = Selected { $count } of { $total }
ui-status-selected-bytes = Selected { $count } of { $total } · { $bytes }
ui-status-free = { $root } { $bytes } free
ui-status-shell-cwd = shell: { $dir }
ui-status-dir = { $name } · { $count } { $count ->
    [one] item
   *[other] items
}

## Frame: tabs, welcome, links (D-02, D-03)
ui-tabs-more = Hidden tabs
ui-keys-tab = Tab
ui-welcome-title = Welcome to Urfin
ui-welcome-keys = Keys
ui-welcome-hint = Alt+letter — search, F10 — quit
ui-welcome-theme = Theme as in Windows
ui-welcome-done = Done
ui-welcome-later = Later
ui-welcome-updated = Urfin updated to { $to } · from { $from }
ui-welcome-news = What's new
ui-keys-preset = Far ↔ DN
ui-keys-flag = Check box
ui-keys-next-field = Next field
ui-link-broken = target not found
ui-size-symlink = <SYMLINK>
ui-size-junction = <JUNCTION>
ui-size-link = <LINK>
# Terminal output highlighting (F03-10): header chip and key.
ui-term-hl-chip = Output highlighting
ui-keys-term-hl = Highlighting
