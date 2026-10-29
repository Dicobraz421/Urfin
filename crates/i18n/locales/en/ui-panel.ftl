# UI texts (S2).
ui-footer-selected = { $count } selected · { $bytes }
ui-footer-items = { $count } { $count ->
    [one] item
   *[other] items
} · { $bytes }
ui-delete-trash = Move { $count } { $count ->
    [one] item
   *[other] items
} ({ $bytes }) to the Recycle Bin?
ui-conflict-head = Conflict { $index } of { $total } · { $job }
ui-loading = Loading…
ui-listing-failed = Cannot read the folder: { $error }
ui-lang-unavailable = Language { $lang } is not available, using English
ui-warn-no-system-font = No system interface font found, using the built-in one
ui-type-folder = Folder
ui-type-file = File
ui-type-file-ext = { $ext } File
ui-type-app = Application
ui-type-archive = { $ext } Archive
ui-type-link = Shortcut
ui-col-name = Name
ui-col-size = Size
ui-col-mtime = Modified
ui-col-ext = Extension
ui-col-type = Type
ui-col-ctime = Created
ui-col-atime = Accessed
ui-col-attrs = Attr.
ui-columns-set = Set “{ $name }”
ui-folder-empty = This folder is empty
ui-retry = Retry
ui-loading-count = Loading… { $count }

# Breadcrumb (S2-12)
ui-menu-loading = Loading…
ui-menu-empty = No folders
ui-path-not-found = Cannot open { $path }: { $error }

ui-renderer-fallback = Graphics driver is failing — switched to software rendering

# Быстрый поиск и начатый аккорд в подвале панели (S3-06)
ui-qs-matches = { $count } found
ui-chord-pending = { $chord } … (waiting for the next key)

# Режимы панели: дерево, сведения, быстрый просмотр (S2-18)
ui-tree-title = Tree · { $root }
ui-tree-title-empty = Tree
ui-tree-hint = ← → collapse/expand · Enter — open
ui-info-title = Information
ui-info-hint = Ctrl+L — close · updates live
ui-info-volume = Volume
ui-info-fs = File system
ui-info-caps = Features
ui-info-free = Free
ui-info-free-value = { $free } of { $total } ({ $pct } %)
ui-info-media-net = network
ui-info-media-usb = removable
ui-info-cap-trash = recycle bin
ui-info-cap-ads = ADS
ui-info-cap-compression = compression
ui-info-cap-clone = block cloning
ui-info-cap-case = case-sensitive
ui-info-path = Path
ui-info-content = Contents
ui-info-content-value = folders: { $dirs } · files: { $files }
ui-info-bytes = Files size
ui-info-selected = Selected
ui-info-cursor = Under cursor
ui-info-name = Name
ui-info-type = Type
ui-info-size = Size
ui-info-mtime = Modified
ui-info-attrs = Attributes
ui-info-kind-dir = folder
ui-info-kind-file = file
ui-info-kind-link = link
ui-info-kind-other = other
ui-qview-hint = F3 — viewer
ui-qview-binary = binary
ui-qview-dir = A folder — its contents are in the panel
ui-info-volume-local = Local Disk

## Диски (D-03)
ui-drives-section-drives = Drives
ui-drives-section-network = Network
ui-drives-section-places = Places
ui-drives-free = { $bytes } free
ui-drives-offline = no connection
ui-place-home = Home
ui-place-desktop = Desktop
ui-place-documents = Documents
ui-place-downloads = Downloads
ui-size-dir = <DIR>

## Состояния папки (D-03)
ui-state-empty = Folder is empty
ui-state-denied = No access to the folder
ui-state-denied-text = Only administrators can read this folder.
ui-state-offline = No connection to { $path }
ui-state-offline-text = The network resource does not respond. Other work does not wait.
ui-state-failed = Cannot read the folder
ui-state-back = Back
ui-state-retry = Retry now

ui-tree-label = tree
ui-qv-title = Quick view
ui-info-summary = Folder { $path }: { $files } files, { $dirs } folders · { $bytes }
ui-info-free-of = { $free } free of { $total }
