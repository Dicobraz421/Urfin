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
# Session recording (F03-12): chip, key, status.
ui-term-rec-chip = Recording { $time }
ui-term-rec-required = required · policy · { $source }
ui-term-rec-stop = Stop
ui-keys-term-rec = Record
ui-term-rec-status = Recording to { $path } · asciicast v2, { $size }
ui-term-rec-starting = Recording · asciicast v2
# SSH multi-execution (F03-11): group screen, chip, status, keys.
ui-group-title = Sessions · input group
ui-group-chip = Input to { $n } { $n ->
    [one] session
   *[other] sessions
}:
ui-group-leave = Leave
ui-group-code = code { $code }
ui-group-status = Group: { $n } of { $total } { $total ->
    [one] session
   *[other] sessions
}
ui-group-confirmed = first command confirmed at { $time }
ui-group-ask = Send to { $n } { $n ->
    [one] session
   *[other] sessions
}:
ui-group-ask-tail = ? Enter — yes, Esc — no
ui-keys-group = Group
ui-keys-group-edit = Group members
ui-keys-group-leave = Leave group
ui-keys-group-toggle = Add / remove
ui-keys-group-send = Send
ui-keys-group-cancel = Do not send
ui-keys-space = Space
