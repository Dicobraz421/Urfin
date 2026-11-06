## File search (S8 §2.5, `screen-search`, `screen-searchpanel`, `screen-searchgrep`).
## $time is a ready string: `du-time-sec` (tenths: 0.4 s), `du-time-min-sec`.
## The match count in the status bar is `ui-find-count`.

# Form
ui-search-name = Name
ui-search-text = Text
ui-search-where = In
ui-search-recursive = Subfolders
ui-search-case-tip = Match case
ui-search-regex-tip = Regular expression

# Result list
ui-search-col-file = File
ui-search-col-match = Match
ui-search-summary = Found { $count } { $count ->
    [one] match
   *[other] matches
} in { $files } { $files ->
    [one] file
   *[other] files
}
ui-search-scanned = scanned { $count } { $count ->
    [one] file
   *[other] files
} · { $time }

# Preview
ui-search-preview-line = line { $line } · preview

# Status bar
ui-search-status = Search “{ $query }” in { $root }
ui-search-status-done = done in { $time }

# Results panel
ui-search-chip = Search
ui-search-query-name = { $mask } in { $root }
ui-search-query-text = “{ $text }” in { $root }
ui-search-panel-progress = found { $found } · scanned { $scanned }
ui-search-panel-total = { $count } { $count ->
    [one] file
   *[other] files
} · { $time }
ui-search-col-line = Line
ui-search-col-hit = First match

# Banners and query errors
ui-search-max-hits = Search stopped: more than { $limit } matches. Narrow the query
ui-search-denied = no access to { $count } { $count ->
    [one] folder
   *[other] folders
}
ui-search-bad-mask = Bad file mask: { $mask }
ui-search-bad-regex = Invalid regular expression: { $error }
ui-search-bad-where = Bad path: { $path }
