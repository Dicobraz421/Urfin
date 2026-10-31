## Viewer (S7 §3, §6): header, status, toasts
ui-view-readonly = read-only
ui-view-enc-auto = auto
ui-view-mode-text = Text
ui-view-mode-markup = Markup
ui-view-mode-hex = Hex
ui-view-mode-markup-off = Markup: unknown language or file larger than { $mb } MB
ui-view-follow = Following
ui-view-to-panels = Back to panels: { $path }
ui-view-lines = Ln { $from }–{ $to } of { $total } · { $pct }%
ui-view-line-approx = ln ≈{ $line }
ui-view-indexing = indexing { $pct }%
ui-view-searching = Searching… { $pct }%
ui-view-wait-index = Waiting for the index… { $pct }% · Esc — cancel
ui-view-offsets = { $from }–{ $to } · { $pct }%
ui-view-offset = Offset { $offset } · { $hex }h = { $dec } · “{ $char }”
ui-view-marks = { $count } { $count ->
    [one] mark
   *[other] marks
} “{ $text }”
ui-view-eol-mixed = Mixed
ui-view-plain = Plain text
ui-view-enc-rereads = Changing the encoding re-reads the file from disk
ui-view-utf32 = UTF-32 isn’t supported — shown as hex
ui-view-copy-limit = Selection is larger than { $mb } MB — not copied
ui-view-truncated = File truncated — showing the end
ui-view-rotated = File was replaced — reopened
ui-view-not-found = Not found: “{ $text }”
ui-view-history-empty = No viewed or edited files yet
ui-view-history-view = View history
ui-view-history-edit = Edit history
ui-view-history-find = Find file
ui-screens-title = Screens
ui-screens-panels = Panels
ui-screens-view = view
ui-screens-edit = edit
ui-view-external-local = External viewer: files on a local disk only
ui-view-bookmark-set = Bookmark { $n } set
ui-view-bookmark-none = No bookmark { $n } — Ctrl+Shift+{ $n } sets it

## Quick view (S7 §4)
ui-qv-lines = ln { $from }–{ $to } of { $total }
ui-qv-cloud = File is in the cloud — F3 to download
ui-qv-dir-size = F3 — folder size

## Find, replace, go to (S7 §3.5, §3.6, §5.2)
ui-find = Find
ui-find-of = { $n } of { $total }
ui-find-of-more = { $n } of { $total }+
ui-find-none = No matches
ui-find-case = Case
ui-find-word = Word
ui-find-regex = Regex
ui-find-hex = Hex
ui-find-wrapped = Continued from the beginning
ui-find-bad-regex = Invalid regular expression: { $error }
ui-find-bad-hex = Hex: bytes like DE AD ?? EF
ui-replace = Replace with
ui-replace-one = Replace
ui-replace-all = Replace all
ui-replace-groups = $1, $2 — groups from “Find”
ui-replace-done = Replaced: { $count }
ui-goto-line = Go to line
ui-goto-offset = Go to offset
ui-goto-of-line = of { $total } · line
ui-goto-of-line-col = of { $total } · line:column
ui-goto-of-offset = of { $total } · offset
ui-goto-invalid = Expected a line, offset (0x1F00, 1F00h) or percent
ui-button-go = Go
ui-keys-find-replace = Find ↔ Replace
ui-keys-select = Select
ui-keys-cycle = Cycle

## Encodings (S7 §3.2, screen-viewerenc)
ui-enc-auto = Auto
ui-enc-auto-hint = detect from content
ui-enc-unicode = Unicode
ui-enc-cyr-windows = Cyrillic, Windows
ui-enc-cyr-dos = Cyrillic, DOS
ui-enc-cyr-unix = Cyrillic, Unix
ui-enc-ukrainian = Ukrainian
ui-enc-cyr-iso = Cyrillic, ISO
ui-enc-cyr-mac = Cyrillic, macOS
ui-enc-western = Western
ui-enc-other = Other

## Editor (S7 §5): header, inline strips, prompts, status
ui-edit-added = +{ $count } added
ui-edit-changed = { $count } changed
ui-edit-removed = −{ $count } removed
ui-edit-loading = Loading… { $pct }%
ui-edit-too-big = File is too large for the editor ({ $size })
ui-edit-open-viewer = Open in viewer
ui-edit-external = External editor
ui-edit-lossy = There are bytes invalid in { $enc }. Saving will replace them.
ui-edit-change-enc = Change encoding
ui-edit-readonly-save = File is read-only. Clear the attribute and save?
ui-edit-clear-readonly = Clear and save
ui-edit-unmappable = { $count } { $count ->
    [one] character
   *[other] characters
} can’t be written in { $enc }
ui-edit-save-utf8 = Save as UTF-8
ui-edit-replace-q = Replace with ?
ui-edit-disk-changed = File changed on disk
ui-edit-reload = Reload
ui-edit-overwrite = Save over
ui-edit-unsaved = Not saved: { $count }
ui-edit-save-all = Save all
ui-edit-dont-save = Don’t save
ui-edit-close-changed = “{ $name }” has changes. Save?
ui-edit-save = Save
ui-edit-cp-on-save = The document has changes: { $enc } will apply on save
ui-edit-remote-save-as = Remote file: save it to a local folder with “Save as”
ui-edit-saving = Saving { $name }
ui-edit-save-failed = Couldn’t save { $name }: { $error }
ui-edit-pos = Ln { $line }, Col { $col }
ui-edit-lines = { $count } { $count ->
    [one] line
   *[other] lines
}
ui-edit-spaces = Spaces: { $n }
ui-edit-tabs = Tab size: { $n }
ui-edit-insert = Insert
ui-edit-overtype = Overtype
ui-edit-md-only = Markdown files only

## Editor toolbar (S7 §5.8, screen-editor, screen-editor-md)
ui-edit-tb-save = Save
ui-edit-tb-undo = Undo
ui-edit-tb-redo = Redo
ui-edit-tb-find = Find
ui-edit-tb-replace = Replace
ui-edit-tb-wrap = Wrap
ui-edit-tb-bold = Bold
ui-edit-tb-italic = Italic
ui-edit-tb-heading = Heading
ui-edit-tb-heading-0 = Normal text
ui-edit-tb-heading-n = Heading { $level }
ui-edit-tb-list = Bulleted list
ui-edit-tb-numbered = Numbered list
ui-edit-tb-task = Task
ui-edit-tb-link = Link
ui-edit-tb-code = Code
ui-edit-tb-table = Table
ui-edit-tb-preview = Preview
ui-edit-tb-encoding = Encoding
ui-edit-tb-eol = Line ending
ui-edit-tb-more = More
ui-edit-tb-no-undo = Nothing to undo
ui-edit-tb-no-redo = Nothing to redo
ui-edit-tb-not-modified = No changes
ui-edit-tb-too-big = File larger than { $mb } MB
ui-edit-tb-readonly = Read-only

## Markdown document (S7 §3.10, §5.9, screen-viewer-md)
ui-md-img = image
ui-md-img-remote = not loaded from the network
ui-md-img-missing = file not found
ui-md-img-not-local = not on a local disk
ui-md-img-format = format not supported
ui-md-img-too-big = larger than { $mb } MB
ui-md-hidden = { $count } { $count ->
    [one] image
   *[other] images
} not shown
ui-md-hidden-remote = { $count } { $count ->
    [one] network image
   *[other] network images
} not shown
ui-md-link-open = Ctrl+click to open in browser
ui-md-details = Details
ui-md-table-col = Column { $n }

## Settings: Viewer (S7 §8, screen-settings-viewer)
ui-set-viewer = Viewer
ui-set-viewer-open = Opening
ui-set-viewer-placement = Open with F3 in
ui-set-viewer-window = Window
ui-set-viewer-panel = Panel
ui-set-viewer-placement-note = panel — like quick view
ui-set-viewer-external = External viewer
ui-set-viewer-external-default = Windows program for the file type
ui-set-viewer-remember = Remember position
ui-set-viewer-remember-on = File position, encoding and mode
ui-set-viewer-remember-note = { $count } latest · Alt+F11
ui-set-viewer-text = Text
ui-set-viewer-codepage = Default encoding
ui-set-viewer-codepage-auto = Auto — by content
ui-set-viewer-codepage-note = F8: { $ring }
ui-set-viewer-wrap = Line wrap
ui-set-viewer-wrap-off = Don’t wrap
ui-set-viewer-wrap-words = By words
ui-set-viewer-wrap-chars = By characters
ui-set-viewer-markdown = Markdown
ui-set-viewer-markdown-document = Document
ui-set-viewer-markdown-source = Source
ui-set-viewer-markdown-note = how .md looks in “Markup” mode
ui-set-tab-size = Tab size
ui-set-viewer-hex-cols = Hex: bytes per row
ui-set-viewer-hex-auto = Auto
ui-set-viewer-hex-note = auto — by window width
ui-set-viewer-large = Large files and logs
ui-set-viewer-mmap = Memory mapping
ui-set-viewer-mmap-on = For files from { $mb } MB on a local disk
ui-set-viewer-mmap-note = except changed in the last { $sec } s
ui-set-viewer-follow = Log following
ui-set-viewer-follow-on = Reopen after rotation
ui-set-viewer-follow-note = Shift+F · check every { $ms } ms

## Settings: Editor (S7 §8, screen-settings-editor)
ui-set-editor = Editor
ui-set-editor-text = Text
ui-set-editor-font = Font
ui-set-editor-font-value = { $family }, { $size }
ui-set-editor-indent = Indentation
ui-set-editor-indent-spaces = Spaces instead of tabs
ui-set-editor-wrap = Line wrap
ui-set-editor-wrap-on = Wrap long lines
ui-set-editor-gutter = Gutter
ui-set-editor-gutter-on = Line numbers, folding, minimap
ui-set-editor-toolbar = Toolbar
ui-set-editor-toolbar-on = Show above the text
ui-set-editor-files = Files
ui-set-editor-new-cp = New file encoding
ui-set-editor-utf8 = UTF-8 without BOM
ui-set-editor-utf8-bom = UTF-8 with BOM
ui-set-editor-eol = Line endings
ui-set-editor-eol-file = As in file
ui-set-editor-highlight = Syntax highlighting
ui-set-editor-highlight-on = On
ui-set-editor-highlight-note = files over { $mb } MB — no highlighting
ui-set-editor-autosave = Autosave
ui-set-editor-autosave-off = Off
ui-set-editor-autosave-focus = When leaving the screen
ui-set-editor-autosave-delay = A second after an edit
