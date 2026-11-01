//! Пакет грамматики Урфина (S7-21a, S7 §5.7): нативная DLL одного языка, которую
//! `urfin_view::syntax` грузит по требованию из `grammars\<язык>.dll` рядом с `urfin.exe`
//! (только папка установки: никакой загрузки из сети и из профиля пользователя).
//!
//! Формат выбран замером S7-21a: рантайм tree-sitter `wasm` (wasmtime) добавляет к exe
//! ≈ 9,6 МиБ, нативная DLL — 0 (загрузчик — `LoadLibraryExW` в `view::sys`), а сами
//! грамматики в `.wasm` и в DLL близки по размеру (таблицы разбора — данные).
//!
//! Экспорт (версия формата — [`ABI`]):
//! - `urfin_grammar_abi() -> u32` — версия формата пакета;
//! - `urfin_grammar() -> *const ()` — `TSLanguage*` (как `tree_sitter_<язык>()`);
//! - `urfin_highlights(index: usize, len: *mut usize) -> *const u8` — части запросов
//!   подсветки (UTF-8, захваты tree-sitter; роли темы по ним выбирает Урфин), null — конец.
//!
//! Сборка одного языка (имя фичи = имя языка `urfin_view::lang` = имя файла):
//!
//! ```text
//! cd assets/syntax/pack
//! cargo build --release --features rust
//! copy target\release\urfin_grammar.dll <папка urfin.exe>\grammars\rust.dll
//! ```

/// Версия формата пакета: меняется вместе с экспортом.
pub const ABI: u32 = 1;

#[cfg(feature = "html")]
mod g {
    pub use tree_sitter_html::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_html::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "css")]
mod g {
    pub use tree_sitter_css::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_css::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "javascript")]
mod g {
    pub use tree_sitter_javascript::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_javascript::HIGHLIGHT_QUERY];
}
#[cfg(feature = "python")]
mod g {
    pub use tree_sitter_python::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_python::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "powershell")]
mod g {
    pub use tree_sitter_powershell::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_powershell::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "diff")]
mod g {
    pub use tree_sitter_diff::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_diff::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "make")]
mod g {
    pub use tree_sitter_make::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_make::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "cmake")]
mod g {
    pub use tree_sitter_cmake::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_cmake::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "lua")]
mod g {
    pub use tree_sitter_lua::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_lua::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "rust")]
mod g {
    pub use tree_sitter_rust::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_rust::HIGHLIGHTS_QUERY];
}
#[cfg(all(feature = "c", not(feature = "cpp")))]
mod g {
    pub use tree_sitter_c::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_c::HIGHLIGHT_QUERY];
}
#[cfg(feature = "go")]
mod g {
    pub use tree_sitter_go::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_go::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "java")]
mod g {
    pub use tree_sitter_java::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_java::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "bash")]
mod g {
    pub use tree_sitter_bash::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_bash::HIGHLIGHT_QUERY];
}
// C++ и TypeScript наследуют запросы C и JavaScript (как в их репозиториях):
// свои шаблоны — первыми, они важнее.
#[cfg(feature = "cpp")]
mod g {
    pub use tree_sitter_cpp::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_cpp::HIGHLIGHT_QUERY, tree_sitter_c::HIGHLIGHT_QUERY];
}
#[cfg(feature = "csharp")]
mod g {
    pub use tree_sitter_c_sharp::LANGUAGE;
    pub const HL: &[&str] = &[tree_sitter_c_sharp::HIGHLIGHTS_QUERY];
}
#[cfg(feature = "typescript")]
mod g {
    pub use tree_sitter_typescript::LANGUAGE_TYPESCRIPT as LANGUAGE;
    pub const HL: &[&str] = &[
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        tree_sitter_javascript::HIGHLIGHT_QUERY,
    ];
}
#[cfg(feature = "tsx")]
mod g {
    pub use tree_sitter_typescript::LANGUAGE_TSX as LANGUAGE;
    pub const HL: &[&str] = &[
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        tree_sitter_javascript::HIGHLIGHT_QUERY,
    ];
}

/// Версия формата пакета.
#[unsafe(no_mangle)]
pub extern "C" fn urfin_grammar_abi() -> u32 {
    ABI
}

/// `TSLanguage*` грамматики.
#[unsafe(no_mangle)]
pub extern "C" fn urfin_grammar() -> *const () {
    // SAFETY: функция грамматики tree-sitter без аргументов возвращает указатель на
    // статическую таблицу языка.
    unsafe { (g::LANGUAGE.into_raw())() }
}

/// Часть `index` запросов подсветки: указатель на UTF-8, длина — в `*len`; за последней
/// частью — null. Части склеиваются через перевод строки, первая важнее (C++ = свои + C).
///
/// # Safety
/// `len` — валидный указатель на `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn urfin_highlights(index: usize, len: *mut usize) -> *const u8 {
    let Some(part) = g::HL.get(index) else {
        return std::ptr::null();
    };
    // SAFETY: контракт функции — `len` валиден.
    unsafe { *len = part.len() };
    part.as_ptr()
}
