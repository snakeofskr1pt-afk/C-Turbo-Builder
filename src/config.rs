//! `turbo.json` — project configuration, parsed with a hand-written JSON reader.
//!
//! No serde: the tool must stay dependency-free. The JSON subset supported is
//! the full RFC 8259 grammar for the value types we need (object, array, string
//! with escapes/surrogates, number, true/false/null).
//!
//! Precedence: command line beats `turbo.json` beats built-in defaults.
//!
//! ```json
//! {
//!   "compiler": "g++",
//!   "flags": ["-O2", "-std=c++20"],
//!   "include": ["include", "third_party/include"],
//!   "defines": { "NDEBUG": null, "ENGINE_VERSION": "42" },
//!   "exclude": ["third_party", "tests"],
//!   "chunkSize": 10,
//!   "jobs": 8,
//!   "out": "app.exe",
//!   "linker": "lld-link.exe",
//!   "linkFlags": ["-lm"]
//! }
//! ```
//!
//! Relative `include` paths are resolved against the directory containing
//! `turbo.json`, so the config keeps working no matter where it is invoked from.

use crate::compiler::Toolchain;
use crate::util;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Minimal JSON parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Parse a complete JSON document. Trailing garbage is rejected.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(format!(
            "unexpected trailing characters at byte {}",
            parser.pos
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, byte: u8, what: &str) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {what} at byte {}", self.pos))
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        match self.peek() {
            None => Err("unexpected end of JSON input".to_string()),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::String(self.parse_string()?)),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(_) => self.parse_number(),
        }
    }

    fn literal(&mut self, text: &str) -> Result<(), String> {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(())
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"', "a string")?;
        let mut out = String::new();
        loop {
            let byte = self.peek().ok_or("unterminated string")?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = self.peek().ok_or("unterminated escape sequence")?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        other => {
                            return Err(format!("invalid escape `\\{}`", other as char));
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(format!(
                        "raw control character in string at byte {}",
                        self.pos
                    ));
                }
                _ => {
                    let start = self.pos;
                    let len = utf8_sequence_len(byte);
                    if start + len > self.bytes.len() {
                        return Err("truncated UTF-8 sequence in string".to_string());
                    }
                    let chunk = &self.bytes[start..start + len];
                    let text = std::str::from_utf8(chunk)
                        .map_err(|_| format!("invalid UTF-8 in string at byte {start}"))?;
                    out.push_str(text);
                    self.pos += len;
                }
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let high = self.parse_hex4()?;
        if (0xD800..=0xDBFF).contains(&high) {
            // Expect the matching low surrogate: \uXXXX
            if self.bytes.get(self.pos) != Some(&b'\\')
                || self.bytes.get(self.pos + 1) != Some(&b'u')
            {
                return Err("lone high surrogate in string".to_string());
            }
            self.pos += 2;
            let low = self.parse_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err("invalid low surrogate in string".to_string());
            }
            let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
            return char::from_u32(code).ok_or_else(|| "invalid surrogate pair".to_string());
        }
        if (0xDC00..=0xDFFF).contains(&high) {
            return Err("lone low surrogate in string".to_string());
        }
        char::from_u32(high).ok_or_else(|| "invalid \\u escape".to_string())
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.bytes.len() {
            return Err("truncated \\u escape".to_string());
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| "invalid \\u escape".to_string())?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| format!("invalid \\u escape `{text}`"))?;
        self.pos += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // JSON forbids leading zeros: `0` or `[1-9][0-9]*`.
        if self.peek() == Some(b'0')
            && matches!(self.bytes.get(self.pos + 1), Some(byte) if byte.is_ascii_digit())
        {
            return Err(format!("invalid number at byte {start} (leading zero)"));
        }
        let mut integer_digits = 0usize;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                self.pos += 1;
                integer_digits += 1;
            } else {
                break;
            }
        }
        if integer_digits == 0 {
            return Err(format!("invalid number at byte {start}"));
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let mut fraction_digits = 0usize;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() {
                    self.pos += 1;
                    fraction_digits += 1;
                } else {
                    break;
                }
            }
            if fraction_digits == 0 {
                return Err(format!("invalid number at byte {start}"));
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            let mut exponent_digits = 0usize;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() {
                    self.pos += 1;
                    exponent_digits += 1;
                } else {
                    break;
                }
            }
            if exponent_digits == 0 {
                return Err(format!("invalid number at byte {start}"));
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "invalid number".to_string())?;
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| format!("invalid number `{text}`"))
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect(b'{', "an object")?;
        self.skip_whitespace();
        let mut entries = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':', "':' after an object key")?;
            self.skip_whitespace();
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(entries));
                }
                _ => {
                    return Err(format!(
                        "expected ',' or '}}' in object at byte {}",
                        self.pos
                    ))
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect(b'[', "an array")?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' in array at byte {}", self.pos)),
            }
        }
    }
}

fn utf8_sequence_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// Config model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub compiler: Option<String>,
    /// Raw compiler flags, passed through verbatim.
    pub flags: Vec<String>,
    /// Directories added as `-I` / `/I` (resolved against the config directory).
    pub include: Vec<String>,
    /// `NAME -> value`; an empty value means a bare `-DNAME`.
    pub defines: Vec<(String, String)>,
    /// Extra directories skipped while discovering sources.
    pub exclude: Vec<String>,
    pub chunk_size: Option<usize>,
    pub jobs: Option<usize>,
    pub out: Option<String>,
    pub linker: Option<String>,
    /// Flags forwarded to the link step only.
    pub link_flags: Vec<String>,
}

impl Config {
    /// Load `path`, returning `Ok(None)` when the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Config>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        let json = parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        Config::from_json(&json)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn from_json(json: &Json) -> Result<Config, String> {
        let entries = match json {
            Json::Object(entries) => entries,
            _ => return Err("root value must be a JSON object".to_string()),
        };
        let mut config = Config::default();
        for (key, value) in entries {
            match key.as_str() {
                "compiler" => config.compiler = Some(expect_string(value, key)?),
                "flags" => config.flags = expect_string_array(value, key)?,
                "linkFlags" => config.link_flags = expect_string_array(value, key)?,
                "include" => config.include = expect_string_array(value, key)?,
                "exclude" => config.exclude = expect_string_array(value, key)?,
                "defines" => config.defines = parse_defines(value)?,
                "chunkSize" => {
                    let size = expect_usize(value, "chunkSize")?;
                    if size == 0 {
                        return Err("`chunkSize` must be at least 1".to_string());
                    }
                    config.chunk_size = Some(size);
                }
                "jobs" => {
                    let jobs = expect_usize(value, "jobs")?;
                    if jobs == 0 {
                        return Err("`jobs` must be at least 1".to_string());
                    }
                    config.jobs = Some(jobs);
                }
                "out" => config.out = Some(expect_string(value, key)?),
                "linker" => config.linker = Some(expect_string(value, key)?),
                other => eprintln!("[turbo] turbo.json: unknown key `{other}` ignored"),
            }
        }
        Ok(config)
    }

    /// Flags contributed by the config: raw flags, then `-I`, then `-D`.
    ///
    /// Include paths are absolute (resolved against `config_dir`) and use
    /// forward slashes, and defines are attached (`-DNAME=VALUE`) so paths with
    /// spaces or backslashes need no quoting.
    pub fn compile_flags(&self, toolchain: Toolchain, config_dir: &Path) -> Vec<String> {
        let mut flags = self.flags.clone();
        for dir in &self.include {
            let absolute = util::absolute(&config_dir.join(dir));
            let literal = util::include_literal(&absolute);
            flags.push(match toolchain {
                Toolchain::Msvc => format!("/I{literal}"),
                Toolchain::Gnu => format!("-I{literal}"),
            });
        }
        for (name, value) in &self.defines {
            let define = if value.is_empty() {
                name.clone()
            } else {
                format!("{name}={value}")
            };
            flags.push(match toolchain {
                Toolchain::Msvc => format!("/D{define}"),
                Toolchain::Gnu => format!("-D{define}"),
            });
        }
        flags
    }
}

fn expect_string(value: &Json, field: &str) -> Result<String, String> {
    match value {
        Json::String(text) => Ok(text.clone()),
        _ => Err(format!("`{field}` must be a string")),
    }
}

fn expect_string_array(value: &Json, field: &str) -> Result<Vec<String>, String> {
    match value {
        Json::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                Json::String(text) => Ok(text.clone()),
                _ => Err(format!("`{field}[{index}]` must be a string")),
            })
            .collect(),
        _ => Err(format!("`{field}` must be an array of strings")),
    }
}

fn expect_usize(value: &Json, field: &str) -> Result<usize, String> {
    match value {
        Json::Number(number) => {
            if !number.is_finite()
                || *number < 0.0
                || number.fract() != 0.0
                || *number > usize::MAX as f64
            {
                return Err(format!("`{field}` must be a non-negative integer"));
            }
            Ok(*number as usize)
        }
        _ => Err(format!("`{field}` must be a number")),
    }
}

fn parse_defines(value: &Json) -> Result<Vec<(String, String)>, String> {
    let entries = match value {
        Json::Object(entries) => entries,
        _ => return Err("`defines` must be an object".to_string()),
    };
    let mut defines = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let text = match value {
            Json::Null => String::new(),
            Json::Bool(true) => "1".to_string(),
            Json::Bool(false) => "0".to_string(),
            Json::String(text) => text.clone(),
            Json::Number(number) => {
                if number.fract() == 0.0 && number.abs() < 1e15 {
                    format!("{}", *number as i64)
                } else {
                    format!("{number}")
                }
            }
            _ => {
                return Err(format!(
                    "`defines.{name}` must be a string, number, boolean or null"
                ))
            }
        };
        defines.push((name.clone(), text));
    }
    Ok(defines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_scalars_and_nesting() {
        let json = parse(r#"{"a":1,"b":[true,false,null],"c":{"d":"x"}}"#).unwrap();
        assert_eq!(json.get("a"), Some(&Json::Number(1.0)));
        assert_eq!(
            json.get("b"),
            Some(&Json::Array(vec![
                Json::Bool(true),
                Json::Bool(false),
                Json::Null
            ]))
        );
        assert_eq!(
            json.get("c").and_then(|c| c.get("d")),
            Some(&Json::String("x".to_string()))
        );
    }

    #[test]
    fn parses_escapes_and_unicode() {
        let json = parse(r#""line\nquote\"slash\\tab\t""#).unwrap();
        assert_eq!(json, Json::String("line\nquote\"slash\\tab\t".to_string()));

        let json = parse(r#""\u0041\u00e9\u2603""#).unwrap();
        assert_eq!(json, Json::String("Aé☃".to_string()));

        // Surrogate pair: U+1F600 GRINNING FACE
        let json = parse(r#""\ud83d\ude00""#).unwrap();
        assert_eq!(json, Json::String("😀".to_string()));
    }

    #[test]
    fn parses_negative_and_fractional_numbers() {
        let json = parse("[-1, 2.5, 1e3, -0.25e-2]").unwrap();
        assert_eq!(
            json,
            Json::Array(vec![
                Json::Number(-1.0),
                Json::Number(2.5),
                Json::Number(1000.0),
                Json::Number(-0.0025)
            ])
        );
    }

    #[test]
    fn rejects_invalid_json() {
        for bad in [
            "",
            "{",
            "{\"a\":}",
            "{\"a\" 1}",
            "[1,]",
            "{\"a\":1,}",
            "tru",
            "01",
            "\"unterminated",
            "{\"a\":1}trailing",
        ] {
            assert!(parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    fn config(json: &str) -> Config {
        Config::from_json(&parse(json).unwrap()).unwrap()
    }

    #[test]
    fn reads_every_field() {
        let config = config(
            r#"{
                "compiler": "cl.exe",
                "flags": ["/O2", "/EHsc"],
                "include": ["include", "third_party/include"],
                "defines": {"NDEBUG": null, "VERSION": "42", "DEBUG": false, "LEVEL": 3},
                "exclude": ["tests"],
                "chunkSize": 10,
                "jobs": 4,
                "out": "app.exe",
                "linker": "link.exe",
                "linkFlags": ["user32.lib"]
            }"#,
        );
        assert_eq!(config.compiler.as_deref(), Some("cl.exe"));
        assert_eq!(config.flags, vec!["/O2", "/EHsc"]);
        assert_eq!(config.include, vec!["include", "third_party/include"]);
        assert_eq!(
            config.defines,
            vec![
                ("NDEBUG".to_string(), String::new()),
                ("VERSION".to_string(), "42".to_string()),
                ("DEBUG".to_string(), "0".to_string()),
                ("LEVEL".to_string(), "3".to_string()),
            ]
        );
        assert_eq!(config.exclude, vec!["tests"]);
        assert_eq!(config.chunk_size, Some(10));
        assert_eq!(config.jobs, Some(4));
        assert_eq!(config.out.as_deref(), Some("app.exe"));
        assert_eq!(config.linker.as_deref(), Some("link.exe"));
        assert_eq!(config.link_flags, vec!["user32.lib"]);
    }

    #[test]
    fn empty_config_has_defaults() {
        let config = config("{}");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn rejects_wrong_types() {
        for bad in [
            r#"{"chunkSize": "ten"}"#,
            r#"{"chunkSize": -1}"#,
            r#"{"chunkSize": 1.5}"#,
            r#"{"chunkSize": 0}"#,
            r#"{"jobs": 0}"#,
            r#"{"compiler": 42}"#,
            r#"{"include": "include"}"#,
            r#"{"include": [1]}"#,
            r#"{"defines": ["X"]}"#,
            r#"{"defines": {"X": []}}"#,
        ] {
            let parsed = parse(bad).unwrap();
            assert!(Config::from_json(&parsed).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn include_paths_are_absolute_and_escaped() {
        let config = config(r#"{"include": ["include", "other/dir"]}"#);
        let base = PathBuf::from("C:/work/proj");
        let flags = config.compile_flags(Toolchain::Gnu, &base);
        assert_eq!(flags.len(), 2);
        assert!(flags[0].starts_with("-I"));
        assert!(flags[1].starts_with("-I"));
        assert!(flags[0].ends_with("include"));
        assert!(flags[1].ends_with("other/dir"));
        assert!(!flags[0].contains('\\'));
    }

    #[test]
    fn defines_become_msvc_and_gnu_flags() {
        let config = config(r#"{"defines": {"NDEBUG": null, "V": "2"}}"#);
        let base = PathBuf::from("C:/p");
        assert_eq!(
            config.compile_flags(Toolchain::Msvc, &base),
            vec!["/DNDEBUG", "/DV=2"]
        );
        assert_eq!(
            config.compile_flags(Toolchain::Gnu, &base),
            vec!["-DNDEBUG", "-DV=2"]
        );
    }

    #[test]
    fn load_missing_file_returns_none() {
        let missing = std::env::temp_dir().join(format!("turbo-no-such-{}", util::unique_token()));
        assert!(Config::load(&missing).unwrap().is_none());
    }
}
