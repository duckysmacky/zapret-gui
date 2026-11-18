//! Strategy `.bat` parsing (D1: parse as text, never execute).

use serde::{Deserialize, Serialize};

/// Variables a strategy may use; anything else makes it unparseable (never guess).
pub const KNOWN_VARIABLES: &[&str] = &[
    "BIN",
    "LISTS",
    "GameFilter",
    "GameFilterTCP",
    "GameFilterUDP",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Strategy {
    /// File name without `.bat`.
    pub name: String,
    /// winws arguments with `%VAR%` placeholders still in place (substituted at launch).
    pub args: Vec<String>,
    pub parse_error: Option<String>,
}

impl Strategy {
    pub fn is_usable(&self) -> bool {
        self.parse_error.is_none()
    }
}

pub fn parse_strategy(name: &str, text: &str) -> Strategy {
    match parse_args(text) {
        Ok(args) => Strategy {
            name: name.to_string(),
            args,
            parse_error: None,
        },
        Err(reason) => Strategy {
            name: name.to_string(),
            args: Vec::new(),
            parse_error: Some(reason),
        },
    }
}

/// Logical lines: CRLF normalized, `^` line continuations joined with a space.
fn logical_lines(text: &str) -> Vec<String> {
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw_line in text.split('\n') {
        let trimmed = raw_line.trim_end();
        if let Some(without_caret) = trimmed.strip_suffix('^') {
            current.push_str(without_caret);
            current.push(' ');
        } else {
            current.push_str(trimmed);
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn parse_args(text: &str) -> Result<Vec<String>, String> {
    let launch_line = logical_lines(text)
        .into_iter()
        .find(|line| {
            let lower = line.trim_start().to_lowercase();
            lower.contains("winws.exe") && !lower.starts_with("::") && !lower.starts_with("rem ")
        })
        .ok_or_else(|| "не найдена строка запуска winws.exe".to_string())?;
    let position = launch_line
        .to_lowercase()
        .find("winws.exe")
        .ok_or_else(|| "не найдена строка запуска winws.exe".to_string())?;
    let rest = &launch_line[position + "winws.exe".len()..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let args = tokenize(rest)?;
    if args.is_empty() {
        return Err("у winws.exe нет аргументов".into());
    }
    for arg in &args {
        check_variables(arg)?;
    }
    Ok(args)
}

/// cmd-style tokenizing: whitespace splits, double quotes group (and are removed),
/// `^` outside quotes escapes the next character.
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quotes = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '^' if !in_quotes => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    has_token = true;
                }
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            ch => {
                current.push(ch);
                has_token = true;
            }
        }
    }
    if in_quotes {
        return Err("незакрытая кавычка".into());
    }
    if has_token {
        tokens.push(current);
    }
    Ok(tokens)
}

fn check_variables(arg: &str) -> Result<(), String> {
    let mut rest = arg;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let end = after
            .find('%')
            .ok_or_else(|| format!("непарный символ % в аргументе {arg}"))?;
        let variable = &after[..end];
        if !KNOWN_VARIABLES.contains(&variable) {
            return Err(format!("неизвестная переменная %{variable}%"));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// Upstream's natural sort: digit runs padded to 8 characters.
pub fn natural_key(name: &str) -> String {
    let mut key = String::new();
    let mut digits = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if !digits.is_empty() {
            key.push_str(&format!("{digits:0>8}"));
            digits.clear();
        }
        key.push(ch);
    }
    if !digits.is_empty() {
        key.push_str(&format!("{digits:0>8}"));
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuations_quotes_and_escapes() {
        let text = "@echo off\r\nset \"BIN=%~dp0bin\\\"\r\nstart \"zapret: %~n0\" /min \"%BIN%winws.exe\" --a=1 ^\r\n--hostlist=\"%LISTS%list general.txt\" --fake-tls=^! --new\r\n";
        let strategy = parse_strategy("test", text);
        assert_eq!(strategy.parse_error, None);
        assert_eq!(
            strategy.args,
            vec![
                "--a=1",
                "--hostlist=%LISTS%list general.txt",
                "--fake-tls=!",
                "--new"
            ]
        );
    }

    #[test]
    fn unknown_variable_is_rejected() {
        let strategy = parse_strategy("bad", "start \"\" \"%BIN%winws.exe\" --x=%SECRET%\n");
        assert_eq!(
            strategy.parse_error.as_deref(),
            Some("неизвестная переменная %SECRET%")
        );
        assert!(strategy.args.is_empty());
        let unpaired = parse_strategy("bad", "start \"\" \"%BIN%winws.exe\" --x=50%\n");
        assert!(unpaired.parse_error.is_some());
    }

    #[test]
    fn missing_launch_line() {
        let strategy = parse_strategy(
            "none",
            "@echo off\r\necho hi\r\n:: winws.exe in a comment\r\n",
        );
        assert!(strategy.parse_error.is_some());
    }

    #[test]
    fn natural_order() {
        let mut names = vec![
            "general (ALT10)",
            "general (ALT2)",
            "general",
            "general (ALT)",
        ];
        names.sort_by_key(|name| natural_key(name));
        assert_eq!(
            names,
            vec![
                "general",
                "general (ALT)",
                "general (ALT2)",
                "general (ALT10)"
            ]
        );
    }
}
