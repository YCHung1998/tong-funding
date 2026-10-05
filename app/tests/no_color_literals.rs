//! design-tokens spec: "UI 程式碼 SHALL NOT 在 theme 模組之外出現十六進位色碼字面值".
//! Scans every source file under `app/src` except the theme module and its tests.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_theme_module(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("theme.rs") | Some("theme_tests.rs")
    )
}

/// Finds `0xRRGGBB` / `0xRRGGBBAA` and `#RRGGBB` / `#RRGGBBAA` style literals.
fn color_literals(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let prefix_len = if bytes[i] == b'#' {
            1
        } else if bytes[i] == b'0' && i + 1 < bytes.len() && (bytes[i + 1] | 0x20) == b'x' {
            2
        } else {
            0
        };
        if prefix_len > 0 {
            let start = i + prefix_len;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            let digits = end - start;
            if digits == 6 || digits == 8 {
                found.push(line[i..end].to_string());
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    found
}

#[test]
fn detector_finds_literals_and_ignores_normal_numbers() {
    assert_eq!(color_literals("let c = 0x0B1016;"), vec!["0x0B1016"]);
    assert_eq!(color_literals("\"#58D3C5\""), vec!["#58D3C5"]);
    assert_eq!(color_literals("0xE5EDF5FF"), vec!["0xE5EDF5FF"]);
    assert!(color_literals("let n = 0x1F; let m = 1234567;").is_empty());
}

#[test]
fn no_color_literals_outside_theme_module() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "scanner found no source files under {src:?}");

    let mut violations = Vec::new();
    for file in files.iter().filter(|f| !is_theme_module(f)) {
        for (n, line) in fs::read_to_string(file).unwrap().lines().enumerate() {
            for lit in color_literals(line) {
                violations.push(format!("{}:{}: {lit}", file.display(), n + 1));
            }
        }
    }
    assert!(violations.is_empty(), "color literals outside theme module:\n{}", violations.join("\n"));
}
