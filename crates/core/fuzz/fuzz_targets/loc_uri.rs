//! `Location::parse_uri`: без паник; `to_uri` читается обратно тем же `Location`;
//! `parent`/`file_name` без паник, `has_parent` согласован с `parent`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use urfin_core::Location;

fuzz_target!(|s: &str| {
    let Ok(l) = Location::parse_uri(s) else {
        return;
    };
    let uri = l.to_uri();
    assert_eq!(Location::parse_uri(&uri).as_ref(), Ok(&l), "uri = {uri:?}");
    assert_eq!(l.has_parent(), l.parent().is_some(), "{l:?}");
    let _ = l.file_name();
    let _ = l.is_local();
});
