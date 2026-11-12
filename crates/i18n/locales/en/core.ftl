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
ops-no-write = { $scheme }: writing is not supported
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
## Operations over SFTP (F03-07): job bar
ops-net-wait = waiting for connection
ops-resume-from = resuming from { $pct } %
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
ops-issues-gone = No such task, or its report is no longer kept
# Skip reasons in the issue list (ops.issues) that have no prompt of their own.
ops-issue-link-skipped = Link skipped
ops-issue-link-dir-remote = Link to a folder on the server — not followed
ops-issue-link-cycle = Link cycle: the folder was already visited
ops-verify-mismatch = copy checksum does not match the source
ops-verify-bad = Unknown copy check: { $what } (off, size, hash)
ops-mr-regex-form = regex: “find/replace”, e.g. ^img_/photo_
ops-mr-cycle-blocked = not renamed: the rename cycle stopped at “{ $name }”
ops-job-multi-rename = Multi-rename
ops-mr-nothing = Nothing to rename: names do not change
ops-job-multi-rename-undo = Undoing multi-rename
ops-mr-undo-nothing = Nothing to undo: no multi-rename yet, or the last one is already undone
ops-mr-skipped = Skipped with problems: { $n }
ops-job-link = Creating a link
ops-link-hard-dir = a hard link can only point to a file
ops-link-junction-file = a junction can only point to a folder
ops-link-no-privilege = a symbolic link needs Windows Developer Mode or administrator rights; a junction works for folders
ops-issue-no-symlink = No rights for a symbolic link
ops-choice-junction = Create junction
ops-link-kind-bad = Unknown link kind: { $what } (hard, sym, junction)
ops-attrs-bad = Unknown attribute: { $what } (R, H, S, A)
ops-mode-bad = Unrecognized permissions: { $what } (e.g. 644 or u+x)
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
# SSH terminal (F03 §3.1): ssh.exe exited with an error.
term-ssh-closed = [connection closed, code { $code }]
term-ssh-reconnect = Enter to reconnect
# Session recording (F03 §3.5, Ctrl+Alt+R).
term-group-denied = Multi-execution is disabled by policy
term-group-no-ssh = No SSH sessions
term-rec-blocked = Recording failed — input blocked
term-rec-required = Recording is required by policy — it cannot be stopped
term-rec-forbidden = Recording SSH sessions is forbidden by policy
term-rec-off = Recording SSH sessions is off in settings ([ssh] recording = "off")
term-rec-failed-blocked = Recording failed — input blocked: { $error }
term-rec-failed = Session recording stopped: { $error }
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

# Disk usage (F-01): handler `du`
du-job = Disk usage: { $path }
du-job-changed = Disk usage, changed folders: { $path }
du-local-only = Disk usage works with local folders
du-not-du = Not a disk usage tab
du-not-yet = Not available yet

# Count as administrator (F01-12, ADR-0021)
du-elevate-job = Counting as administrator: { $count ->
    [one] { $count } folder
   *[other] { $count } folders
}
du-elevate-uac = waiting for UAC confirmation
du-elevate-running = Counting as administrator…
du-elevate-declined = Cancelled in UAC — nothing was counted
du-elevate-done = Counted as administrator: { $count ->
    [one] { $count } folder
   *[other] { $count } folders
}, { $size }{ $left ->
    [0] {""}
   *[other] {" "}· still no access: { $left }
}
du-elevate-failed = Could not count as administrator: { $reason }
du-elevate-exit = helper exit code { $code }
du-elevate-locked = urfin.exe or a library next to it is open for writing in another program — close it and try again
du-elevate-policy = Counting as administrator is disabled by organization policy
du-elevate-nothing = No folders without access
du-elevate-unavailable = Counting as administrator is not available in this mode

# Disk usage filters (F01-08): Types groups, Temporary
du-type-video = Video
du-type-audio = Audio
du-type-image = Images
du-type-doc = Documents
du-type-archive = Archives
du-type-disk = Disk images
du-type-exec = Programs
du-type-code = Code
du-type-temp = Temporary
du-type-other = Other
du-type-build = Builds and caches
du-dups-job = Duplicates: comparing file heads
du-dups-job-full = Duplicates: verifying by hash
du-types-more = { $count } more { $count ->
    [one] file
   *[other] files
}
du-temp-in = In { $root }
du-temp-system = System and profile
du-temp-what-temp = programs' temporary files
du-temp-what-wu = Windows Update downloads
du-temp-what-do = update delivery cache
du-temp-what-dumps = crash dumps and error reports
du-temp-what-thumbs = thumbnail cache
du-temp-what-browser = browser cache
du-temp-what-devcache = developer package cache
du-temp-what-recycle = Recycle Bin — empty it there
du-temp-what-cargo = Cargo builds
du-temp-what-npm = npm packages
du-temp-what-file = temporary file
du-temp-what-wintemp = Windows and service temporary files
du-temp-label-temp = %TEMP%
du-temp-label-wu = Windows Update downloads
du-temp-label-do = Delivery Optimization cache
du-temp-label-dumps = Memory dumps
du-temp-label-wer = Windows error reports
du-temp-label-thumbs = Thumbnail cache
du-temp-label-chrome = Google Chrome cache
du-temp-label-edge = Microsoft Edge cache
du-temp-label-brave = Brave cache
du-temp-label-firefox = Firefox cache
du-temp-label-npm = npm cache
du-temp-label-pip = pip cache
du-temp-label-nuget = NuGet packages
du-temp-label-cargo = Cargo downloads
du-temp-label-yarn = Yarn cache
du-temp-label-recycle = Recycle Bin
du-effect-safe = safe
du-effect-recreated = rebuilt
du-effect-loses = data loss
du-temp-after-temp = safe: files in use by running programs are skipped
du-temp-after-wu = safe: Windows downloads them again if needed
du-temp-after-dumps = crash reports for troubleshooting are lost
du-temp-after-thumbs = Explorer thumbnails reappear gradually
du-temp-after-browser = sign-ins and tabs stay, pages load slower at first
du-temp-after-devcache = packages are downloaded again on next install (network needed)
du-temp-after-recycle = deleted items can no longer be restored — empty it in the Recycle Bin itself
du-temp-after-cargo = rebuilt on the next build — this takes long
du-temp-after-npm = restored by npm install — network needed, takes long
du-temp-after-cache = program caches are rebuilt, first start is slower
du-temp-after-file = safe if the program that created it is closed
du-clean-title = Clean up temporary files?
du-clean-body = To Recycle Bin: { $count } { $count ->
    [one] row
   *[other] rows
}, frees { $size }.
du-clean-note = • { $effect }: { $after } — { $count } · { $size }
du-clean-busy = Files in use are skipped; deleted items can be restored from the Recycle Bin.
du-clean-trash = To Recycle Bin
du-clean-cancel = Cancel
du-temp-denied = no access: { $count } { $count ->
    [one] folder
   *[other] folders
}
