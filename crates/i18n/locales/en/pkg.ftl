# Package managers (urfin-pkg, F02).
pkg-not-yet = Not available yet: { $what }
# choco changes the system only with administrator rights: UAC prompt before the action (F02 §8).
pkg-choco-uac = choco requires administrator rights — a UAC prompt will follow
# Job state when the user declines the UAC prompt.
pkg-uac-declined = declined
# Elevated choco keeps running after cancel: our process cannot stop it.
pkg-cancel-elevated = cancelled, choco may continue in its window

## Packages tab `pkg:` (F02-08, `screen-pkginstalled`, `screen-pkgupdates`).
pkg-chip = Packages
pkg-tab = pkg:
pkg-col-name = Program
pkg-col-version = Version
pkg-col-available = Available
pkg-col-mgr = Manager
pkg-col-publisher = Publisher, source
# Installed program that no manager tracks (winget sees it in Apps without a source).
pkg-outside = not managed
# scoop has no publisher: the bucket the app came from.
pkg-bucket = bucket { $name }
pkg-filter-installed = Installed
pkg-filter-updates = Updates
pkg-filter-search = Find
# Right side of the tab header.
pkg-count = { $count ->
    [one] { $count } program
   *[other] { $count } programs
}
pkg-of = { $n } of { $total }
# Status bar: when the managers were last asked.
pkg-checked-today = Checked today at { $time }
pkg-checked-on = Checked { $date } at { $time }
pkg-checking = Checking…
pkg-never = Not checked yet
pkg-no-managers = No package managers found
# A manager whose last query failed: name and error.
pkg-failed = { $mgr }: { $error }
pkg-important = important
# Bottom strip of Updates with marked rows.
pkg-upgrade-n = Upgrade { $count }
pkg-upgrade = Upgrade
pkg-upgrade-all = Upgrade all { $count }
pkg-cancel = Cancel
# Key hints of the tab.
pkg-key-upgrade = Upgrade
pkg-key-uninstall = Uninstall
pkg-key-check = Check for updates
pkg-key-output = Output in terminal
pkg-key-mark = Mark
pkg-key-upgrade-marked = Upgrade marked
pkg-key-jobs = Jobs

## Search (F02-09, `screen-pkgsearch`, `screen-pkgmgrpick`).
pkg-col-name-search = Program · package
pkg-col-source = Source
pkg-col-publisher-search = Publisher
# The variant of the first manager in `[pkg].order`.
pkg-recommended = recommended
pkg-search-title = Find package
# "Find package [7zip] in [All ▾]".
pkg-search-in = in
pkg-search-all = All
pkg-search-go = Find
# Managers pouch: "All" searches every manager at once.
pkg-search-parallel = in parallel
pkg-search-absent = not found on this PC
pkg-search-order = Order: { $order }
# Answers under the field: "winget · 4", "scoop searching…", "choco no answer".
pkg-search-busy = searching…
pkg-search-failed = no answer
pkg-search-empty = Nothing found
pkg-search-none = No package managers found
pkg-search-chip = Find “{ $q }”
pkg-search-note = { $groups ->
    [one] { $groups } program
   *[other] { $groups } programs
} · { $variants ->
    [one] { $variants } variant
   *[other] { $variants } variants
}
pkg-key-results = To results
pkg-key-manager = Manager
pkg-key-close = Close line
pkg-key-install-marked = Install marked
pkg-key-choose = Choose
pkg-key-apply = Apply
pkg-key-close-list = Close list

## Actions (F02-10, `screen-pkginstall`).
pkg-busy-install = installing…
pkg-busy-upgrade = upgrading…
pkg-busy-uninstall = uninstalling…
pkg-busy-queued = queued
pkg-busy-uac = waiting for UAC consent
pkg-busy-uac-maybe = possibly waiting for UAC consent
pkg-silent = no output for { $min } min
pkg-feed-title = Packages
pkg-feed-exit = exit code { $code }
pkg-feed-stopped = stopped: { $error }
pkg-retry-nothing = Nothing to retry
pkg-quit-title = Installing: { $n }
pkg-quit-body = “Abort and quit” stops the package manager and its installer; the package may stay partly installed
pkg-quit-wait = Wait
pkg-quit-abort = Abort and quit
# F8 confirmation in one line.
pkg-uninstall-one = Uninstall { $name } ({ $mgr })?
pkg-uninstall-n = Uninstall { $count ->
    [one] { $count } package
   *[other] { $count } packages
}?
pkg-uninstall-yes = Uninstall
# Batch job title: "winget upgrade · 3 packages".
pkg-job-many = { $cmd } · { $count ->
    [one] { $count } package
   *[other] { $count } packages
}
pkg-reboot = Restart the computer to finish the installation
pkg-later = Later
# Background check banner (F02-11, `screen-pkgbanner`): "7 updates available · 2 important".
pkg-banner-updates = { $count ->
    [one] { $count } update available
   *[other] { $count } updates available
}
pkg-banner-important = { $count } important
pkg-banner-scheduled-today = checked on schedule today at { $time }
pkg-banner-scheduled-on = checked on schedule { $date } at { $time }
pkg-banner-manual-today = checked today at { $time }
pkg-banner-manual-on = checked { $date } at { $time }
pkg-banner-show = Show
# Check now (settings) and scheduled check (Task Scheduler).
pkg-check-none = No updates available
pkg-check-busy = An update check is already running
pkg-check-failed = Update check failed: { $error }
pkg-schedule-on = Scheduled update check: daily at { $at }
pkg-schedule-off = Scheduled update check is off
pkg-schedule-failed = Task Scheduler: { $error }
pkg-schedule-bad-at = Time is HH:MM, e.g. 09:30

## Раздел настроек «Пакеты» (F02-12, `screen-settings-pkg`).
pkg-set-section = Packages
pkg-set-managers = Managers
pkg-set-order = Order
pkg-set-order-note = first one wins when a program matches
pkg-set-missing = not found
pkg-set-search = Default search
pkg-set-search-all = All detected managers
pkg-set-search-note = in parallel
pkg-set-updates = Updates
pkg-set-schedule = Scheduled check
pkg-set-schedule-on = Check
pkg-set-daily = Daily
pkg-set-at = at
pkg-set-check-now = Check now
pkg-set-schedule-note = Windows Task Scheduler
pkg-set-task-other = The task points to another path: { $path }
pkg-set-task-absent = The task is missing from Task Scheduler
pkg-set-task-fix = Update
pkg-set-important = Important programs
pkg-set-important-note = top of the list and in the banner
pkg-set-important-none = not found among installed
pkg-set-run = Run
pkg-set-output = Manager output
pkg-set-output-term = In the terminal on request
pkg-set-admin = Administrator rights
pkg-set-admin-uac = Requested by the manager itself (UAC)
# Контекстное меню строки вкладки `pkg:`.
pkg-menu-important = Important
