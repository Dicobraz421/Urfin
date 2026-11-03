# Баннер-вопрос, конфликт и черновики (S2-15)
ui-prompt-more = { $count ->
    [one] { $count } more question
   *[other] { $count } more questions
}
ui-draft-ops-multi_rename = New names:
ui-mr-pattern-error = Pattern: { $error }
ui-mr-problem-invalid = invalid name
ui-mr-problem-duplicate = duplicate
ui-mr-problem-exists = name taken
ui-mr-summary = { $changed } will change, { $problems } with problems (skipped) · { $shown } shown · [N] name, [E] extension, [C:1:1:3] counter, [Y][M][D] date, [U]/[L] case, “find/replace” — regex
ui-draft-ops-link = Link in:
ui-draft-ops-attrs = Attributes (+R −H +S +A):
ui-draft-ops-touch = Modified time:
ui-prompt-all = For all
ui-prompt-all-hint = Space; Shift+key answers for all
ui-prompt-new-name = New name
ui-button-apply = Apply
ui-button-back = Back
ui-button-later = Later
ui-button-cancel = Cancel
ui-button-delete = Delete
ui-button-permanent = Permanently
ui-conflict-name = Name
ui-conflict-size = Size
ui-conflict-mtime = Modified
ui-conflict-new = New
ui-conflict-old = Existing
ui-conflict-newer = ▲ newer by { $span }
ui-conflict-older = ▼ older by { $span }
ui-span-min = { $n } min
ui-span-hours = { $n } h
ui-span-days = { $n } d
ui-draft-delete = Move { $count } { $count ->
    [one] item
   *[other] items
} ({ $size }) to the Recycle Bin?
ui-draft-delete-permanent = Delete { $count } { $count ->
    [one] item
   *[other] items
} ({ $size }) PERMANENTLY? This cannot be undone.
ui-draft-copy = Copy { $count } { $count ->
    [one] item
   *[other] items
} ({ $size }) to
ui-draft-move = Move { $count } { $count ->
    [one] item
   *[other] items
} ({ $size }) to
ui-draft-ops-mkdir = New folder
ui-draft-sel-add = Select by mask
ui-draft-sel-remove = Deselect by mask
ui-draft-panel-filter = Filter

# Переименование по месту (S2-16)
ui-rename-hint = Enter — rename · Esc — cancel

## Встроенные строки (D-04)
ui-strip-ifexists = If the file exists
ui-strip-ifexists-ask = Ask
ui-strip-ifexists-overwrite = Replace
ui-strip-ifexists-skip = Skip
ui-strip-ifexists-newer = Replace if newer
ui-strip-keep-dates = Keep dates
ui-strip-keep-perms-dates = Keep permissions and dates
ui-strip-verify = Verify hash after copying
ui-strip-verify-move = Verify hash before deleting the source
ui-strip-background = In background
ui-strip-matches = { $count } match
ui-strip-mr-summary = will change { $changed } · problems { $problems }
ui-strip-conflict-of = { $name } · conflict { $index } of { $total }
ui-strip-newer = newer
ui-strip-mkdir = New folder
ui-strip-mkdir-many = Several, separated by ;
ui-strip-mkdir-nested = nested — with \
ui-strip-filter = Show
ui-strip-filter-matches = matches { $count } of { $total }
ui-strip-attrs = { $count ->
    [one] Attributes of { $count } item
   *[other] Attributes of { $count } items
}
ui-strip-attrs-mtime = Modified
ui-strip-attrs-now = now
ui-strip-attrs-varies = various
ui-attr-readonly = Read-only
ui-attr-hidden = Hidden
ui-attr-system = System
ui-attr-archive = Archive
ui-filter-chip = filter { $mask }
ui-panel-count-filtered = { $count } of { $total }
ui-button-create = Create
ui-button-reset = Reset
ui-keys-apply = Apply
ui-keys-cancel = Cancel
ui-keys-name-ext = Name ↔ extension
ui-button-copy = Copy
ui-button-move = Move
ui-button-mark = Select
ui-button-unmark = Deselect
ui-button-rename = Rename
ui-button-ok = OK
ui-keys-qs-next = Next
ui-keys-qs-prev = Previous
ui-keys-close = Close

## Field completion (No. 70)
ui-complete-history = history
ui-complete-folder = folder
ui-complete-here = in folder
ui-keys-complete-next = Next
ui-keys-insert = Insert
ui-keys-field-history = Field history

## Operation strips (D-04): link, time, multi-rename, F7, compare, sync
ui-strip-link = Link
ui-strip-link-to = to
ui-strip-link-kind = Type
ui-strip-link-hint = Without Developer Mode this needs administrator rights; for a folder a junction works
ui-link-sym = Symbolic
ui-link-hard = Hard
ui-link-junction = Junction
ui-strip-touch = Modified time
ui-strip-touch-deep = Including contents
ui-strip-touch-hint = created and accessed — in Attributes
ui-strip-mr = New names
ui-mr-tok-name = name
ui-mr-tok-ext = extension
ui-mr-tok-counter = counter
ui-mr-tok-date = date
ui-mr-tok-replace = find/replace with /
ui-mkdir-exists = exists
ui-mkdir-new = new
ui-strip-sync = Synchronize:
ui-strip-sync-what = { $count } { $count ->
    [one] file
   *[other] files
} · { $size }
ui-strip-sync-mirror = Delete extra
ui-strip-sync-mirror-server = Delete extra on server
ui-strip-sync-extra-server = extra on server: { $n }
ui-strip-sync-go = Synchronize
ui-compare-title = Folder comparison:
ui-compare-only-left = only on the left { $n }
ui-compare-only-right = only on the right { $n }
ui-compare-newer-left = newer on the left { $n }
ui-compare-newer-right = newer on the right { $n }
ui-compare-differs = different { $n }
ui-compare-equal = identical { $n }
ui-compare-sync-lr = Synchronize →
ui-compare-sync-rl = Synchronize ←
ui-button-close = Close
ui-tag-newer = newer
ui-tag-only-here = only here
ui-prompt-skip-all = Skip all such
ui-job-waiting-for = waiting for an answer: { $what }
