# Core texts (urfin-core, crate::text::tr).
core-unknown-command = Unknown command: { $id }
core-command-failed = { $id }: { $error }
core-task-panicked = Task { $task } failed unexpectedly
core-not-found = Not found: { $name }
core-no-match = Nothing matches { $mask }
core-no-bookmark = Bookmark { $n } is empty
core-last-tab = Cannot close the last tab
core-no-tab = No tab { $n }
core-config-warnings = Settings file has problems ({ $count }): { $first }
core-crashed = Urfin closed unexpectedly last time. Details: { $file }
core-action-show-logs = Show logs
core-action-dismiss = Dismiss
vfs-job-dir-size = Directory sizes
vfs-dir-size-none = No folders to measure: put the cursor on a folder or select folders
ops-not-yet = Not available yet: { $what }
ops-nothing = Nothing to do: put the cursor on a file or select files
ops-dest-empty = Destination is empty
ops-dest-file = Destination is a file: { $path }
ops-dest-mkdir-failed = Cannot create { $path }: { $error }
ops-conflict-bad = Unknown conflict policy: { $what }
ops-filter-bad = Bad file mask: { $mask }
ops-job-copy = { $n ->
    [one] Copy { $name }
   *[other] Copy { $n } items
}
ops-job-move = { $n ->
    [one] Move { $name }
   *[other] Move { $n } items
}
ops-copy-same = Cannot copy a file onto itself
ops-copy-into-self = Cannot copy a folder into itself
ops-copy-type = A file and a folder have the same name
ops-copy-no-name = No free name for a copy
ops-move-src-left = Copied, but the source was not deleted: { $error }
ops-move-size = Copy is { $got } B instead of { $want } B — source kept
ops-move-flush = Copy not flushed to disk: { $error } — source kept
ops-copy-job-failed = Operation failed: { $error }
ops-copy-errors = { $n ->
    [one] 1 item failed
   *[other] { $n } items failed
}: { $first }
ops-conflict-exists = File already exists
ops-conflict-other = Could not process the file
ops-conflict-body = Conflict { $index } of { $total }: { $name } — { $cmp }
ops-conflict-body-short = Conflict { $index } of { $total }: { $name }
ops-conflict-newer = newer
ops-conflict-older = older
ops-conflict-same-time = same time
ops-conflict-larger = larger by { $size }
ops-conflict-smaller = smaller by { $size }
ops-conflict-same-size = same size
ops-issue-body = Problem { $index } of { $total }: { $name } — { $error }
ops-issue-permission = Access denied
ops-issue-sharing = File in use
ops-issue-busy = Device is busy or not ready
ops-issue-network = Network is unavailable
ops-issue-not-found = Not found
ops-issue-other = Operation failed
ops-issue-low-space = Not enough disk space
ops-choice-retry = Retry
ops-retry-nothing = Nothing to retry: no errors, or the task report is no longer kept
ops-recover-banner = Interrupted on exit: { $what } → { $dest }
ops-recover-banner-pct = Interrupted on exit: { $what } → { $dest } · ≈ { $n } % done
ops-recover-more = { $n } more
ops-issue-holder = open in { $exe } (PID { $pid })
ops-issue-code = code { $code }
ops-issue-of = { $index } of { $total }
ops-recover-continue = Continue
ops-recover-clear = Clear
ops-quit-title = Operations in progress ({ $n })
ops-quit-body = “Abort and exit” stops the tasks; you can continue them on the next start
ops-quit-wait = Wait
ops-quit-abort = Abort and exit
ops-issue-no-trash = Cannot move to Recycle Bin — it would be deleted permanently
ops-no-trash-why = larger than the Recycle Bin, a network drive, or a path too long
ops-choice-delete-permanently = Delete permanently
ops-low-space-detail = needs ≈{ $need }, { $free } free
ops-issue-too-large = File larger than 4 GB — does not fit this drive (FAT32)
ops-too-large-why = FAT32 cannot store files over 4 GB; format the drive as exFAT or NTFS
ops-issue-invalid-name = A name Windows cannot open
ops-invalid-name-fix = fixed name: { $name }
ops-choice-fix-name = Fix name
ops-job-gone = The task has already finished
ops-verify-mismatch = copy checksum does not match the source
ops-verify-bad = Unknown copy check: { $what } (off, size, hash)
ops-mr-regex-form = regex: “find/replace”, e.g. ^img_/photo_
ops-mr-cycle-blocked = not renamed: the rename cycle stopped at “{ $name }”
ops-job-multi-rename = Multi-rename
ops-mr-nothing = Nothing to rename: names do not change
ops-mr-skipped = Skipped with problems: { $n }
ops-job-link = Creating a link
ops-link-hard-dir = a hard link can only point to a file
ops-link-junction-file = a junction can only point to a folder
ops-link-no-privilege = a symbolic link needs Windows Developer Mode or administrator rights; a junction works for folders
ops-issue-no-symlink = No rights for a symbolic link
ops-choice-junction = Create junction
ops-link-kind-bad = Unknown link kind: { $what } (hard, sym, junction)
ops-attrs-bad = Unknown attribute: { $what } (R, H, S, A)
ops-touch-bad = Cannot read the time: { $what } (e.g. 2026-10-21 18:05 or now)
ops-compare-result = Compare: only left { $only_left }, only right { $only_right }, newer left { $newer_left }, newer right { $newer_right }, differ { $differs }, equal { $equal }
ops-sync-nothing = Nothing to synchronize: nothing to copy to the other panel
ops-sync-bad = Unknown sync direction: { $what } (lr, rl)
ops-choice-overwrite = Replace
ops-choice-skip = Skip
ops-choice-rename = Rename
ops-choice-newer = Replace if newer
ops-choice-cancel = Cancel
ops-copy-skipped = { $n ->
    [one] 1 item skipped
   *[other] { $n } items skipped
}
ops-open-failed = Cannot open { $name }: { $error }
core-settings-not-saved = Settings not saved to { $file }: { $error }
ops-mkdir-failed = Cannot create { $name }: { $error }
ops-rename-failed = Cannot rename { $name }: { $error }
ops-exists = { $name } already exists
ops-rename-bad = A name cannot contain \ or / — use Move (F6) to put it elsewhere
ops-job-trash = Moving to Recycle Bin
ops-job-delete = Deleting
ops-delete-failed = Not deleted: { $error }
term-not-yet = Terminal: { $what } is not done yet (S6)

## Task titles for the job bar (D-06): "what → where", the UI writes the kind
ops-job-items = { $n ->
    [one] { $name }
   *[other] { $n } items
}
ops-job-transfer = { $n ->
    [one] { $name }
   *[other] { $n } items
} → { $dest }
ops-job-trash-what = { $n ->
    [one] { $name }
   *[other] { $n } items
} → Recycle Bin

term-exited = [process exited, code { $code }]
term-exited-unknown = [process exited]
term-start-failed = Cannot start { $shell }: { $error }
term-unknown-shell = Unknown shell: { $id }
term-no-shells = No shells found
term-no-session = No terminal session
term-quit-title = Running processes in the terminal ({ $n })
term-quit-body = “Terminate and exit” closes the terminal sessions and their programs
term-quit-cancel = Cancel
term-quit-terminate = Terminate and exit
term-paste-title = Paste { $n } lines into { $shell }
term-paste-body = { $shell } runs every line at once, without a review
term-paste-insert = Paste
term-paste-cancel = Cancel
term-integ-silent = { $shell }: shell integration did not answer (Clink or a custom prompt?) — the terminal works without it
term-complete-dir = folder
term-complete-file = file
term-complete-command = command
term-complete-history = history
term-open-nothing = No file under the cursor
term-open-failed = Cannot open { $name }: { $error }
term-history-title = History · { $shell }
term-history-all = History · all shells
term-history-empty = Nothing found
term-history-keys = Enter insert · Shift+Enter run · Del delete · Ins pin · Ctrl+R all shells
term-history-pinned = pinned
term-history-native = imported
