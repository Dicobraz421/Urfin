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
