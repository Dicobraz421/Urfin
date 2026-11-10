## Archives as folders (S8 §3, `screen-archive`, `screen-extract`, `screen-extractpw`).
## $size, $packed, $limit are ready strings (`fmt-bytes`); $pct is an integer percent.
## Shared with F5: "If file exists" and its choices — `ui-strip-ifexists*` (ui-strip.ftl),
## In background — `ui-strip-background`, Cancel — `ui-button-cancel`, Tab — `ui-keys-next-field`.

# Panel inside an archive (`screen-archive`)
arc-col-packed = Packed
# Status bar: «name · { arc-status-packed } · date»
arc-status-packed = { $size }, packed to { $packed }
arc-summary = archive { $size } · compression { $pct } %

# Extract strip, Shift+F2 (`screen-extract`)
arc-extract-title = Extract { $name } · { $size } to
arc-extract-title-items = Extract { $count } { $count ->
    [one] item
   *[other] items
} from { $name } · { $size } to
arc-extract-title-many = Extract { $count } { $count ->
    [one] archive
   *[other] archives
} · { $size } to
arc-extract-subdir = Into subfolder { $name }
arc-extract-subdir-each = Each into its own subfolder
arc-extract-keep-paths = Keep paths
arc-extract-go = Extract
arc-keys-path-history = Path history

# Password (`screen-extractpw`, §3.7); $method — AES-256, AES-128, ZipCrypto
arc-pw-label = Password
arc-pw-names-encrypted = { $method } · names encrypted
arc-pw-wrong = Wrong password — { $left } { $left ->
    [one] attempt
   *[other] attempts
} left
arc-pw-exhausted = Wrong password — no attempts left
arc-pw-ask = Password for { $name }

# Zip bombs and spill to a temporary folder (§3.4)
arc-spill-ask = Extract { $size } to a temporary folder?
arc-spill-big = more than the { $limit } limit
arc-spill-ratio = compression { $ratio }:1 — looks like a zip bomb
arc-spill-go = Extract

# Errors
arc-error-format = Unknown or unsupported archive format
arc-error-corrupt = The archive is damaged
arc-error-multivolume = Multi-volume archives are not supported yet
arc-error-too-deep = Archives nested deeper than { $max }
arc-error-crc = CRC error: { $name }
arc-error-no-password = The archive is encrypted: a password is needed

# F4 inside an archive (§3.5)
arc-toast-readonly = The archive is read-only — opened in the viewer

# Tasks and the `arc.test` result
arc-job-index = Reading archive
arc-job-extract = Extracting
arc-job-test = Testing archive
arc-test-ok = Tested { $count } { $count ->
    [one] file
   *[other] files
} — no errors
arc-test-failed = Errors in { $errors } { $errors ->
    [one] file
   *[other] files
} of { $count }
