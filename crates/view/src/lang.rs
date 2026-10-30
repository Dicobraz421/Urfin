//! Язык документа по имени файла, расширению и shebang — имя языка gpui-kit
//! (`highlighter::Language::name`) или своей грамматики (`powershell`, `xml`, `ini`;
//! S7 §5.7). Неизвестный — `None`, без подсветки. Есть ли грамматика в сборке, решает UI
//! (сегмент «Разметка» — `text.disabled`); здесь — только имя. Карточка S7-08.

/// Файлы по полному имени (без учёта регистра).
const NAMES: &[(&str, &str)] = &[
    ("makefile", "make"),
    ("gnumakefile", "make"),
    ("cmakelists.txt", "cmake"),
    (".gitignore", "ini"),
    (".gitattributes", "ini"),
    (".gitconfig", "ini"),
    (".gitmodules", "ini"),
    (".editorconfig", "ini"),
    (".npmrc", "ini"),
    ("cargo.lock", "toml"),
    ("pipfile", "toml"),
    ("dockerfile", "bash"),
    (".bashrc", "bash"),
    (".bash_profile", "bash"),
    (".profile", "bash"),
    (".zshrc", "bash"),
    ("gemfile", "ruby"),
    ("rakefile", "ruby"),
];

/// Расширения (без точки, нижний регистр).
const EXTS: &[(&str, &str)] = &[
    ("json", "json"),
    ("jsonc", "json"),
    ("json5", "json"),
    ("toml", "toml"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("ini", "ini"),
    ("cfg", "ini"),
    ("conf", "ini"),
    ("inf", "ini"),
    ("reg", "ini"),
    ("properties", "ini"),
    ("xml", "xml"),
    ("xsd", "xml"),
    ("xsl", "xml"),
    ("xslt", "xml"),
    ("svg", "xml"),
    ("csproj", "xml"),
    ("vcxproj", "xml"),
    ("props", "xml"),
    ("targets", "xml"),
    ("resx", "xml"),
    ("config", "xml"),
    ("manifest", "xml"),
    ("plist", "xml"),
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("html", "html"),
    ("htm", "html"),
    ("xhtml", "html"),
    ("css", "css"),
    ("scss", "css"),
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("jsx", "javascript"),
    ("py", "python"),
    ("pyw", "python"),
    ("pyi", "python"),
    ("ps1", "powershell"),
    ("psm1", "powershell"),
    ("psd1", "powershell"),
    ("diff", "diff"),
    ("patch", "diff"),
    ("mk", "make"),
    ("mak", "make"),
    ("cmake", "cmake"),
    ("lua", "lua"),
    ("rs", "rust"),
    ("c", "c"),
    ("h", "c"),
    ("go", "go"),
    ("java", "java"),
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("cpp", "cpp"),
    ("cc", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "cpp"),
    ("hh", "cpp"),
    ("hxx", "cpp"),
    ("cs", "csharp"),
    ("ts", "typescript"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    ("tsx", "tsx"),
    ("sql", "sql"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("swift", "swift"),
    ("php", "php"),
    ("rb", "ruby"),
    ("proto", "proto"),
    ("graphql", "graphql"),
    ("gql", "graphql"),
    ("zig", "zig"),
    ("scala", "scala"),
    ("ex", "elixir"),
    ("exs", "elixir"),
];

/// Интерпретаторы shebang (`#!/usr/bin/env python3` → `python`).
const SHEBANG: &[(&str, &str)] = &[
    ("python", "python"),
    ("bash", "bash"),
    ("sh", "bash"),
    ("zsh", "bash"),
    ("dash", "bash"),
    ("ksh", "bash"),
    ("node", "javascript"),
    ("deno", "typescript"),
    ("lua", "lua"),
    ("ruby", "ruby"),
    ("php", "php"),
    ("pwsh", "powershell"),
    ("powershell", "powershell"),
    ("make", "make"),
];

/// Язык по имени файла (`name` — последний компонент пути) и началу файла (`head` —
/// первые байты, для shebang). Порядок: полное имя, расширение, shebang.
pub fn detect(name: &str, head: &[u8]) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, l)) = NAMES.iter().find(|(n, _)| *n == lower) {
        return Some(l);
    }
    if lower.starts_with("dockerfile.") || lower.starts_with("makefile.") {
        let base = lower.split('.').next().unwrap_or("");
        return NAMES.iter().find(|(n, _)| *n == base).map(|(_, l)| *l);
    }
    if let Some((_, ext)) = lower.rsplit_once('.')
        && let Some((_, l)) = EXTS.iter().find(|(e, _)| *e == ext)
    {
        return Some(l);
    }
    shebang(head)
}

/// Язык по первой строке `#!…`: имя программы или первый аргумент `env`.
pub fn shebang(head: &[u8]) -> Option<&'static str> {
    let rest = head.strip_prefix(b"#!")?;
    let end = memchr::memchr(b'\n', rest).unwrap_or(rest.len()).min(256);
    let line = std::str::from_utf8(&rest[..end]).ok()?.trim();
    let mut words = line.split_ascii_whitespace();
    let mut prog = words.next()?.rsplit(['/', '\\']).next()?;
    if prog == "env" {
        prog = words.find(|w| !w.starts_with('-'))?;
    }
    // python3.12 → python, bash5 → bash.
    let stem = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    SHEBANG.iter().find(|(p, _)| *p == stem).map(|(_, l)| *l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_ext_and_shebang() {
        assert_eq!(detect("Makefile", b""), Some("make"));
        assert_eq!(detect("CMakeLists.txt", b""), Some("cmake"));
        assert_eq!(detect(".gitignore", b""), Some("ini"));
        assert_eq!(detect("Dockerfile.dev", b""), Some("bash"));
        assert_eq!(detect("README.MD", b""), Some("markdown"));
        assert_eq!(detect("a.tar.json", b""), Some("json"));
        assert_eq!(detect("build.ps1", b""), Some("powershell"));
        assert_eq!(
            detect("run", b"#!/usr/bin/env python3\nprint()"),
            Some("python")
        );
        assert_eq!(detect("run", b"#!/bin/bash -e\n"), Some("bash"));
        assert_eq!(
            detect("run", b"#! /usr/bin/env -S node --x\n"),
            Some("javascript")
        );
        assert_eq!(detect("notes.txt", b"hello"), None);
        assert_eq!(detect("data.bin", b"#!"), None);
        assert_eq!(detect("noext", b""), None);
    }
}
