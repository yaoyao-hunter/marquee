//! The theme layer: colours and attributes for the scrolled text.
//!
//! A [`Theme`] is pure data — what the text should look like, never how to
//! draw it. This module owns everything the *user* writes: the built-in
//! table, the colour names, and the theme file a user authors to define
//! their own themes. It knows nothing about terminals or escape sequences:
//! [`crate::renderer`] turns a theme into bytes, so the configuration layer
//! and the drawing layer stay decoupled — a new theme needs no code change.
//!
//! # Theme files
//!
//! `--theme-file PATH` (or the default location) holds a small TOML subset:
//! one `[section]` per theme, `fg`/`bg` as a colour name, `#rrggbb`, or a
//! *list* of colours, the four attributes as booleans, and `band` as a
//! column count:
//!
//! ```toml
//! # my themes
//! [alert]
//! fg = "bright-red"
//! bold = true
//!
//! [ocean]
//! fg = "#7fd4ff"
//! bg = "blue"
//! dim = true
//!
//! [sunset]
//! fg = ["#ff5e62", "#ff9966", "#ffd194"]
//! band = 4
//! ```
//!
//! A list is a palette: it cycles across the screen's display columns, so
//! the scrolling text flows through the colour bands. `band` sets how many
//! columns each palette entry spans (default 1); one entry is a solid
//! colour.
//!
//! User themes are merged over the built-ins by name, so `[alarm]` in a
//! theme file replaces the built-in `alarm`. `--theme NAME` then picks any
//! theme from the merged registry.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A colour a theme can ask the terminal for: one of the 16 ANSI colours
/// (classic and bright) or a 24-bit `#rrggbb` value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Rgb {
        red: u8,
        green: u8,
        blue: u8,
    },
}

impl Color {
    /// Parses a colour from its theme-file spelling: a name (`red`,
    /// `Bright-Red`, `bright red` — spacing, hyphens and underscores are
    /// equivalent, case is ignored) or a `#rgb` / `#rrggbb` hex value.
    pub fn parse(text: &str) -> Result<Self, String> {
        let normalized = text.trim().to_lowercase().replace([' ', '_'], "-");
        let color = match normalized.as_str() {
            "black" => Self::Black,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "magenta" => Self::Magenta,
            "cyan" => Self::Cyan,
            "white" => Self::White,
            "bright-black" => Self::BrightBlack,
            "bright-red" => Self::BrightRed,
            "bright-green" => Self::BrightGreen,
            "bright-yellow" => Self::BrightYellow,
            "bright-blue" => Self::BrightBlue,
            "bright-magenta" => Self::BrightMagenta,
            "bright-cyan" => Self::BrightCyan,
            "bright-white" => Self::BrightWhite,
            _ => return Self::parse_hex(text, &normalized),
        };
        Ok(color)
    }

    fn parse_hex(text: &str, normalized: &str) -> Result<Self, String> {
        let Some(hex) = normalized.strip_prefix('#') else {
            return Err(format!(
                "unknown colour {text:?}: expected a named colour (red, bright-green, …) or #rrggbb"
            ));
        };
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| {
                c.to_digit(16)
                    .map(|d| d as u8)
                    .ok_or_else(|| format!("unknown colour {text:?}: {hex:?} is not hexadecimal"))
            })
            .collect::<Result<_, _>>()?;
        match digits.as_slice() {
            [r, g, b] => Ok(Self::Rgb {
                red: r * 17,
                green: g * 17,
                blue: b * 17,
            }),
            [r1, r2, g1, g2, b1, b2] => Ok(Self::Rgb {
                red: r1 * 16 + r2,
                green: g1 * 16 + g2,
                blue: b1 * 16 + b2,
            }),
            _ => Err(format!(
                "unknown colour {text:?}: a hex colour is #rgb or #rrggbb, got {hex:?}"
            )),
        }
    }
}

/// What the scrolled text looks like: colours and attributes only. All
/// fields are optional; a theme with everything off (the [`Default`]) is
/// exactly today's monochrome marquee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Foreground palette: empty = the terminal's default, one entry = a
    /// solid colour, more = the entries cycle across the columns.
    pub fg: Vec<Color>,
    /// Background palette of the line, shaped like [`Theme::fg`].
    pub bg: Vec<Color>,
    /// How many display columns each palette entry spans; the width of one
    /// colour band. Only meaningful for a palette of two or more entries.
    pub band: usize,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: Vec::new(),
            bg: Vec::new(),
            band: 1,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }
}

impl Theme {
    /// Whether the theme would change anything on screen; the renderer
    /// skips colour entirely for a theme that does not.
    pub fn is_plain(&self) -> bool {
        self.fg.is_empty()
            && self.bg.is_empty()
            && !self.bold
            && !self.dim
            && !self.italic
            && !self.underline
    }
}

/// The built-in themes, in help/error order.
pub fn builtins() -> Vec<(String, Theme)> {
    fn fg(color: Color) -> Theme {
        Theme {
            fg: vec![color],
            ..Theme::default()
        }
    }
    fn fg_bold(color: Color) -> Theme {
        Theme {
            fg: vec![color],
            bold: true,
            ..Theme::default()
        }
    }
    /// The palette of a list of hex colours.
    fn hexes(colors: &[&str]) -> Vec<Color> {
        colors
            .iter()
            .map(|text| Color::parse(text).expect("a built-in colour parses"))
            .collect()
    }
    /// A theme whose foreground is a list of hex colours.
    fn palette(colors: &[&str]) -> Theme {
        Theme {
            fg: hexes(colors),
            ..Theme::default()
        }
    }
    vec![
        ("default".into(), Theme::default()),
        (
            "bold".into(),
            Theme {
                bold: true,
                ..Theme::default()
            },
        ),
        ("alarm".into(), fg_bold(Color::BrightRed)),
        ("gold".into(), fg_bold(Color::BrightYellow)),
        ("matrix".into(), fg_bold(Color::BrightGreen)),
        ("ice".into(), fg(Color::BrightCyan)),
        ("violet".into(), fg_bold(Color::BrightMagenta)),
        // The banded ones: palettes that cycle across the columns.
        (
            "rainbow".into(),
            Theme {
                fg: vec![
                    Color::BrightRed,
                    Color::BrightYellow,
                    Color::BrightGreen,
                    Color::BrightCyan,
                    Color::BrightBlue,
                    Color::BrightMagenta,
                ],
                ..Theme::default()
            },
        ),
        ("sunset".into(), palette(&["#ff5e62", "#ff9966", "#ffd194"])),
        ("ocean".into(), palette(&["#00c3ff", "#0072ff", "#00e5c0"])),
        (
            "neon".into(),
            Theme {
                fg: hexes(&["#ff2975", "#00f3ff", "#8f5aff"]),
                bg: vec![Color::Black],
                ..Theme::default()
            },
        ),
    ]
}

/// The themes available to `--theme`: the built-ins with any user themes
/// merged over them by name.
#[derive(Debug)]
pub struct Registry {
    entries: Vec<(String, Theme)>,
}

impl Registry {
    /// The built-in themes alone.
    pub fn builtins() -> Self {
        Self {
            entries: builtins(),
        }
    }

    /// Merges `user` themes over the current entries: a theme whose name
    /// already exists replaces it in place, a new name is appended.
    pub fn extend(&mut self, user: Vec<(String, Theme)>) {
        for (name, theme) in user {
            match self
                .entries
                .iter_mut()
                .find(|(existing, _)| *existing == name)
            {
                Some(slot) => slot.1 = theme,
                None => self.entries.push((name, theme)),
            }
        }
    }

    /// The theme `--theme` asked for, if the registry knows it.
    pub fn lookup(&self, name: &str) -> Option<&Theme> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == name)
            .map(|(_, theme)| theme)
    }

    /// The known theme names, in registry order, for error messages.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(name, _)| name.as_str()).collect()
    }
}

/// Why a theme could not be resolved: the file could not be read or
/// parsed, or the requested name is not in the registry.
#[derive(Debug)]
pub enum ThemeError {
    /// Reading the theme file failed.
    Read { path: PathBuf, err: io::Error },
    /// A line of the theme file is not valid.
    Parse { line: usize, message: String },
    /// The same theme name is defined twice in one file.
    Duplicate { name: String },
    /// `--theme` asked for a name the registry does not know.
    UnknownTheme {
        name: String,
        available: Vec<String>,
    },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, err } => write!(f, "cannot read theme file {path:?}: {err}"),
            Self::Parse { line, message } => write!(f, "theme file line {line}: {message}"),
            Self::Duplicate { name } => write!(f, "theme file defines {name:?} twice"),
            Self::UnknownTheme { name, available } => write!(
                f,
                "unknown theme {name:?}; available themes: {}",
                available.join(", ")
            ),
        }
    }
}

impl std::error::Error for ThemeError {}

/// The default theme-file location: `$XDG_CONFIG_HOME/marquee/themes.toml`,
/// or `~/.config/marquee/themes.toml` when XDG is not set. `None` when
/// neither variable can be trusted.
pub fn default_theme_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() && Path::new(&xdg).is_absolute() => PathBuf::from(xdg),
        _ => {
            let home = std::env::var_os("HOME")?;
            if home.is_empty() {
                return None;
            }
            PathBuf::from(home).join(".config")
        }
    };
    Some(base.join("marquee").join("themes.toml"))
}

/// Parses a theme file: one `[name]` section per theme, `fg`/`bg` colour
/// values (a single colour or a `[list]` palette), the four boolean
/// attributes, the `band` column count, `#` comments, blank lines.
/// This is the whole grammar — deliberately the subset of TOML a theme
/// file needs, so no format surprises and no dependency.
pub fn parse_theme_file(text: &str) -> Result<Vec<(String, Theme)>, ThemeError> {
    let mut themes: Vec<(String, Theme)> = Vec::new();
    let mut current: Option<(String, Theme)> = None;

    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let name = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .map(str::trim);
        if let Some(name) = name {
            if name.is_empty() {
                return Err(ThemeError::Parse {
                    line: line_number,
                    message: "a theme name cannot be empty".into(),
                });
            }
            if let Some(theme) = current.take() {
                themes.push(theme);
            }
            if themes.iter().any(|(existing, _)| existing == name) {
                return Err(ThemeError::Duplicate { name: name.into() });
            }
            current = Some((name.to_string(), Theme::default()));
            continue;
        }

        let (key, value) = split_key_value(line, line_number)?;
        let theme = current.as_mut().ok_or(ThemeError::Parse {
            line: line_number,
            message: format!("{key} = … appears before any [theme] section"),
        })?;
        let theme = &mut theme.1;
        match key {
            "fg" | "bg" => {
                let colors = parse_colors(value).map_err(|message| ThemeError::Parse {
                    line: line_number,
                    message,
                })?;
                if key == "fg" {
                    theme.fg = colors;
                } else {
                    theme.bg = colors;
                }
            }
            "band" => {
                let band: usize = value_token(value).parse().map_err(|_| ThemeError::Parse {
                    line: line_number,
                    message: format!("band must be a number of columns, got {value:?}"),
                })?;
                if band == 0 {
                    return Err(ThemeError::Parse {
                        line: line_number,
                        message: "band must be at least 1 column".into(),
                    });
                }
                theme.band = band;
            }
            "bold" | "dim" | "italic" | "underline" => {
                let enabled = parse_bool(value_token(value)).ok_or_else(|| ThemeError::Parse {
                    line: line_number,
                    message: format!("{key} must be true or false, got {value:?}"),
                })?;
                match key {
                    "bold" => theme.bold = enabled,
                    "dim" => theme.dim = enabled,
                    "italic" => theme.italic = enabled,
                    _ => theme.underline = enabled,
                }
            }
            unknown => {
                return Err(ThemeError::Parse {
                    line: line_number,
                    message: format!(
                        "unknown key {unknown:?}: expected fg, bg, band, bold, dim, italic or underline"
                    ),
                });
            }
        }
    }

    if let Some(theme) = current {
        themes.push(theme);
    }
    Ok(themes)
}

/// `key = value`, both trimmed, with the equals sign required.
fn split_key_value(line: &str, line_number: usize) -> Result<(&str, &str), ThemeError> {
    let Some((key, value)) = line.split_once('=') else {
        return Err(ThemeError::Parse {
            line: line_number,
            message: format!("expected [name] or key = value, got {line:?}"),
        });
    };
    Ok((key.trim(), value.trim()))
}

/// The value of a key, without its quotes and without a trailing comment.
fn value_token(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote) {
            return rest.split_once(quote).map_or(rest, |(token, _)| token);
        }
    }
    value
        .split_once(" #")
        .map_or(value, |(token, _)| token)
        .trim_end()
}

/// `true` or `false`, nothing else.
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The value of `fg`/`bg`: one colour, or a `[c1, c2, …]` list — a palette
/// that cycles across the columns. Single-line lists; a trailing comment
/// after the closing `]` is fine.
fn parse_colors(value: &str) -> Result<Vec<Color>, String> {
    let value = value.trim();
    if !value.starts_with('[') {
        return Ok(vec![Color::parse(value_token(value))?]);
    }
    let close = value.rfind(']').ok_or_else(|| {
        format!("a colour list starts with [ and ends with ] on the same line, got {value:?}")
    })?;
    let inner = &value[1..close];
    let mut colors = Vec::new();
    for token in inner.split(',') {
        let token = value_token(token.trim());
        if token.is_empty() {
            continue;
        }
        colors.push(Color::parse(token)?);
    }
    if colors.is_empty() {
        return Err(format!(
            "a colour list needs at least one colour, got {value:?}"
        ));
    }
    Ok(colors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_names_cover_the_16_ansi_colours_and_normalise_spacing() {
        for (text, color) in [
            ("black", Color::Black),
            ("Red", Color::Red),
            ("BRIGHT-RED", Color::BrightRed),
            ("bright green", Color::BrightGreen),
            ("bright_blue", Color::BrightBlue),
            ("Bright-White", Color::BrightWhite),
        ] {
            assert_eq!(Color::parse(text), Ok(color), "{text}");
        }
    }

    #[test]
    fn hex_colours_parse_in_both_lengths() {
        assert_eq!(
            Color::parse("#ff8800"),
            Ok(Color::Rgb {
                red: 0xff,
                green: 0x88,
                blue: 0x00
            })
        );
        // #abc means aa bb cc, like CSS.
        assert_eq!(
            Color::parse("#AbC"),
            Ok(Color::Rgb {
                red: 0xaa,
                green: 0xbb,
                blue: 0xcc
            })
        );
        assert_eq!(
            Color::parse(" #123456 "),
            Ok(Color::Rgb {
                red: 0x12,
                green: 0x34,
                blue: 0x56
            })
        );
    }

    #[test]
    fn bad_colours_say_what_was_expected() {
        for bad in ["chartreuse", "#ff", "#12g456", ""] {
            let err = Color::parse(bad).expect_err("{bad:?} should not parse");
            assert!(err.contains("colour"), "{err}");
        }
        let err = Color::parse("#12g456").unwrap_err();
        assert!(err.contains("not hexadecimal"), "{err}");
        let err = Color::parse("#12345").unwrap_err();
        assert!(err.contains("#rgb or #rrggbb"), "{err}");
    }

    #[test]
    fn the_default_theme_is_plain_and_the_builtins_are_not() {
        assert!(Theme::default().is_plain());
        assert_eq!(
            builtins()
                .iter()
                .filter(|(name, theme)| name == "default" || !theme.is_plain())
                .count(),
            builtins().len()
        );
        let table = builtins();
        let names: Vec<&str> = table.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "default", "bold", "alarm", "gold", "matrix", "ice", "violet", "rainbow", "sunset",
                "ocean", "neon"
            ]
        );
        let rainbow = table
            .iter()
            .find(|(name, _)| name == "rainbow")
            .map(|(_, theme)| theme)
            .expect("rainbow is built in");
        assert_eq!(rainbow.fg.len(), 6, "a six-colour palette: {rainbow:?}");
        assert_eq!(rainbow.band, 1);
        let sunset = table
            .iter()
            .find(|(name, _)| name == "sunset")
            .map(|(_, theme)| theme)
            .expect("sunset is built in");
        assert!(
            sunset
                .fg
                .iter()
                .any(|color| matches!(color, Color::Rgb { .. })),
            "the hex palette parses: {sunset:?}"
        );
    }

    #[test]
    fn a_palette_of_colours_parses_with_the_rest_of_the_grammar() {
        let file = r##"
            [morning]
            fg = ["#ff5e62", '#ff9966', "#ffd194"]   # quotes and comments
            bg = [black, black]
            band = 4

            [single]
            fg = red
        "##;
        let themes = parse_theme_file(file).expect("a valid file parses");
        assert_eq!(themes.len(), 2);

        let (name, morning) = &themes[0];
        assert_eq!(name, "morning");
        assert_eq!(
            morning,
            &Theme {
                fg: vec![
                    Color::parse("#ff5e62").expect("hex parses"),
                    Color::parse("#ff9966").expect("hex parses"),
                    Color::parse("#ffd194").expect("hex parses"),
                ],
                bg: vec![Color::Black, Color::Black],
                band: 4,
                ..Theme::default()
            }
        );

        let (name, single) = &themes[1];
        assert_eq!(name, "single");
        assert_eq!(
            single,
            &Theme {
                fg: vec![Color::Red],
                ..Theme::default()
            }
        );
    }

    #[test]
    fn a_theme_file_defines_and_overrides_themes() {
        let file = r#"
            # leading comment
            [alert]
            fg = "bright-red"   # trailing comment
            bold = true

            [ocean]
            fg = '#7fd4ff'
            bg = blue
            dim = true
            italic = false
            underline = true
        "#;
        let themes = parse_theme_file(file).expect("a valid file parses");
        assert_eq!(themes.len(), 2);

        let (name, alert) = &themes[0];
        assert_eq!(name, "alert");
        assert_eq!(
            alert,
            &Theme {
                fg: vec![Color::BrightRed],
                bold: true,
                ..Theme::default()
            }
        );

        let (name, ocean) = &themes[1];
        assert_eq!(name, "ocean");
        assert_eq!(
            ocean,
            &Theme {
                fg: vec![Color::Rgb {
                    red: 0x7f,
                    green: 0xd4,
                    blue: 0xff
                }],
                bg: vec![Color::Blue],
                dim: true,
                italic: false,
                underline: true,
                bold: false,
                ..Theme::default()
            }
        );
    }

    #[test]
    fn user_themes_merge_over_the_builtins_by_name() {
        let mut registry = Registry::builtins();
        registry.extend(parse_theme_file("[alarm]\nfg = cyan").expect("parses"));
        assert_eq!(
            registry.lookup("alarm"),
            Some(&Theme {
                fg: vec![Color::Cyan],
                ..Theme::default()
            }),
            "the user definition wins"
        );
        registry.extend(vec![("mine".into(), Theme::default())]);
        assert!(registry.lookup("mine").is_some(), "new names are added");
        assert_eq!(registry.lookup("default"), Some(&Theme::default()));
        assert_eq!(
            registry.names(),
            vec![
                "default", "bold", "alarm", "gold", "matrix", "ice", "violet", "rainbow", "sunset",
                "ocean", "neon", "mine"
            ]
        );
    }

    #[test]
    fn every_malformed_line_is_rejected_with_its_line_number() {
        for (file, expect) in [
            ("fg = red", "appears before any [theme] section"),
            ("[a]\nwhat = 1", "unknown key"),
            ("[a]\nfg = sparkles", "unknown colour"),
            ("[a]\nfg = [sparkles]", "unknown colour"),
            ("[a]\nfg = [red", "starts with [ and ends with ]"),
            ("[a]\nfg = []", "at least one colour"),
            ("[a]\nband = 0", "at least 1 column"),
            ("[a]\nband = wide", "band must be a number"),
            ("[a]\nbold = yes", "must be true or false"),
            ("[a]\nbold", "expected [name] or key = value"),
            ("[a]\nfg red", "expected [name] or key = value"),
            ("[]", "name cannot be empty"),
        ] {
            let err = parse_theme_file(file).expect_err("{file:?} should not parse");
            let message = err.to_string();
            let line = file.lines().count();
            assert!(message.contains(&line.to_string()), "{file:?}: {message}");
            assert!(message.contains(expect), "{file:?}: {message}");
        }
    }

    #[test]
    fn a_duplicate_section_name_is_rejected() {
        let err = parse_theme_file("[alarm]\nfg = red\n[alarm]\nbold = true")
            .expect_err("a name may only be defined once");
        assert!(err.to_string().contains("twice"), "{err}");
        assert!(matches!(err, ThemeError::Duplicate { .. }));
    }

    #[test]
    fn an_unknown_theme_lists_what_is_available() {
        let registry = Registry::builtins();
        let err = match registry.lookup("nope") {
            Some(_) => panic!("nope is not a theme"),
            None => ThemeError::UnknownTheme {
                name: "nope".into(),
                available: registry.names().into_iter().map(str::to_string).collect(),
            },
        };
        let message = err.to_string();
        assert!(message.contains("nope"), "{message}");
        assert!(message.contains("matrix"), "{message}");
    }
}
