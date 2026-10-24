/// Validate before creating an output file. Windows path syntax must never be
/// interpreted as a drive prefix, device, or alternate data stream.
pub(super) fn validate(name: &str, windows: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 4096
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\', '\0']),
        "Invalid filename"
    );
    if windows {
        anyhow::ensure!(
            !name.ends_with(['.', ' '])
                && !name
                    .chars()
                    .any(|c| c <= '\u{1f}' || "<>:\"|?*".contains(c)),
            "Filename is not supported on Windows"
        );
        let stem = name
            .split('.')
            .next()
            .unwrap()
            .trim_end_matches(' ')
            .to_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        anyhow::ensure!(!reserved, "Reserved Windows filename");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_windows_special_paths_without_restricting_unix_names() {
        for name in [
            "C:outside",
            "file:stream",
            "NUL.txt",
            "con",
            "COM1",
            "LPT².txt",
            "name.",
            "name ",
            "a?b",
            "a\nb",
        ] {
            assert!(validate(name, true).is_err(), "{name:?}");
        }
        for name in ["文件.txt", "company.txt", "COM10.txt", "normal.bin"] {
            assert!(validate(name, true).is_ok());
        }
        assert!(validate("file:stream", false).is_ok());
        for name in ["", ".", "..", "../file", "a\\b", "a\0b"] {
            assert!(validate(name, false).is_err());
        }
    }
}
