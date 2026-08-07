use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ═══════════════════════════════════════════════════════════════════════════════
// CENTRALIZED COLOR TOKEN SYSTEM
// ═══════════════════════════════════════════════════════════════════════════════
//
// All color definitions live here. Display code references these tokens instead
// of scattering `if use_true { .truecolor(...) } else { .ansi_color() }` branches.

/// ANSI color variants used as fallback when truecolor is unavailable.
#[derive(Debug, Clone, Copy)]
pub enum Ansi {
    White,
    BrightWhite,
    BrightBlack,
    Red,
    Green,
    Yellow,
    Cyan,
    Magenta,
    BrightBlue,
    BrightCyan,
    BrightYellow,
    BrightMagenta,
}

/// A color token pairing an RGB truecolor value with an ANSI fallback.
#[derive(Debug, Clone, Copy)]
pub struct ColorToken {
    pub rgb: (u8, u8, u8),
    pub ansi: Ansi,
}

// ═══════════════════════════════════════════════════════════════════════════════
// ANSI dispatch macro -- must be defined before first use in impl block
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "colors")]
macro_rules! apply_ansi {
    ($text:expr, $ansi:expr $(, $modifier:ident)*) => {
        {
            use owo_colors::OwoColorize;
            match $ansi {
                Ansi::White => $text.white()$(.$modifier())*.to_string(),
                Ansi::BrightWhite => $text.bright_white()$(.$modifier())*.to_string(),
                Ansi::BrightBlack => $text.bright_black()$(.$modifier())*.to_string(),
                Ansi::Red => $text.red()$(.$modifier())*.to_string(),
                Ansi::Green => $text.green()$(.$modifier())*.to_string(),
                Ansi::Yellow => $text.yellow()$(.$modifier())*.to_string(),
                Ansi::Cyan => $text.cyan()$(.$modifier())*.to_string(),
                Ansi::Magenta => $text.magenta()$(.$modifier())*.to_string(),
                Ansi::BrightBlue => $text.bright_blue()$(.$modifier())*.to_string(),
                Ansi::BrightCyan => $text.bright_cyan()$(.$modifier())*.to_string(),
                Ansi::BrightYellow => $text.bright_yellow()$(.$modifier())*.to_string(),
                Ansi::BrightMagenta => $text.bright_magenta()$(.$modifier())*.to_string(),
            }
        }
    };
}

#[cfg(feature = "colors")]
fn no_color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

impl ColorToken {
    pub const fn new(rgb: (u8, u8, u8), ansi: Ansi) -> Self {
        Self { rgb, ansi }
    }

    /// Apply color to text. Truecolor when `tc` is true, ANSI fallback otherwise.
    #[cfg(feature = "colors")]
    pub fn paint(&self, text: &str, tc: bool) -> String {
        if no_color_enabled() {
            return text.to_string();
        }
        if tc {
            use owo_colors::OwoColorize;
            text.truecolor(self.rgb.0, self.rgb.1, self.rgb.2)
                .to_string()
        } else {
            apply_ansi!(text, self.ansi)
        }
    }

    #[cfg(not(feature = "colors"))]
    pub fn paint(&self, text: &str, _tc: bool) -> String {
        text.to_string()
    }

    /// Apply color + bold to text.
    #[cfg(feature = "colors")]
    pub fn bold(&self, text: &str, tc: bool) -> String {
        if no_color_enabled() {
            return text.to_string();
        }
        if tc {
            use owo_colors::OwoColorize;
            text.truecolor(self.rgb.0, self.rgb.1, self.rgb.2)
                .bold()
                .to_string()
        } else {
            apply_ansi!(text, self.ansi, bold)
        }
    }

    #[cfg(not(feature = "colors"))]
    pub fn bold(&self, text: &str, _tc: bool) -> String {
        text.to_string()
    }

    /// Apply color + dimmed. In truecolor mode the RGB value is already muted,
    /// so we just apply it plain. In ANSI mode we add `.dimmed()`.
    #[cfg(feature = "colors")]
    pub fn dim(&self, text: &str, tc: bool) -> String {
        if no_color_enabled() {
            return text.to_string();
        }
        if tc {
            use owo_colors::OwoColorize;
            text.truecolor(self.rgb.0, self.rgb.1, self.rgb.2)
                .to_string()
        } else {
            apply_ansi!(text, self.ansi, dimmed)
        }
    }

    #[cfg(not(feature = "colors"))]
    pub fn dim(&self, text: &str, _tc: bool) -> String {
        text.to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// COLOR TOKEN CONSTANTS -- grouped by domain
// ═══════════════════════════════════════════════════════════════════════════════

// -- Model identity -----------------------------------------------------------
pub const MODEL_FABLE: ColorToken = ColorToken::new((255, 140, 190), Ansi::Magenta);
pub const MODEL_OPUS: ColorToken = ColorToken::new((200, 160, 255), Ansi::BrightMagenta);
pub const MODEL_SONNET: ColorToken = ColorToken::new((255, 200, 100), Ansi::BrightYellow);
pub const MODEL_HAIKU: ColorToken = ColorToken::new((100, 220, 255), Ansi::BrightCyan);
pub const MODEL_GPT_SOL: ColorToken = ColorToken::new((255, 122, 89), Ansi::Red);
pub const MODEL_GPT_TERRA: ColorToken = ColorToken::new((105, 219, 148), Ansi::Green);
pub const MODEL_GPT_LUNA: ColorToken = ColorToken::new((125, 170, 255), Ansi::BrightBlue);

// -- Semantic -----------------------------------------------------------------
pub const SUCCESS: ColorToken = ColorToken::new((134, 239, 172), Ansi::Green);
pub const WARNING: ColorToken = ColorToken::new((253, 224, 71), Ansi::Yellow);
pub const ERROR: ColorToken = ColorToken::new((248, 113, 113), Ansi::Red);
pub const MUTED: ColorToken = ColorToken::new((148, 163, 184), Ansi::BrightBlack);
pub const ACCENT: ColorToken = ColorToken::new((96, 165, 250), Ansi::BrightBlue);

// -- Effort (heat gradient) ---------------------------------------------------
pub const EFFORT_NONE: ColorToken = ColorToken::new((148, 163, 184), Ansi::BrightBlack);
pub const EFFORT_LOW: ColorToken = ColorToken::new((100, 220, 255), Ansi::Cyan);
pub const EFFORT_MEDIUM: ColorToken = ColorToken::new((255, 255, 255), Ansi::BrightWhite);
pub const EFFORT_HIGH: ColorToken = ColorToken::new((255, 200, 100), Ansi::Yellow);
pub const EFFORT_MAX: ColorToken = ColorToken::new((255, 120, 200), Ansi::Magenta);

// -- Primary text -------------------------------------------------------------
pub const PRIMARY: ColorToken = ColorToken::new((255, 255, 255), Ansi::BrightWhite);
pub const PRIMARY_DIM: ColorToken = ColorToken::new((255, 255, 255), Ansi::White);

// ═══════════════════════════════════════════════════════════════════════════════
// GRADIENT -- dynamic color from value/max
// ═══════════════════════════════════════════════════════════════════════════════

// Gradient endpoint colors (green -> yellow -> red)
const GRADIENT_LOW: (u8, u8, u8) = (134, 239, 172);
const GRADIENT_MID: (u8, u8, u8) = (253, 224, 71);
const GRADIENT_HIGH: (u8, u8, u8) = (248, 113, 113);

// ═══════════════════════════════════════════════════════════════════════════════
// EFFORT -- one tier scale shared by the footer and the agent panel
// ═══════════════════════════════════════════════════════════════════════════════

/// Paint an effort label on the shared none..max tier scale.
///
/// Both surfaces route through here so a given label can never read differently
/// in the footer than in the agent panel. `med` and `ultracode` are the aliases
/// Claude Code itself accepts for effort input, so an operator who sets
/// `CLAUDE_CODE_EFFORT_LEVEL` to either still gets a correctly placed chip;
/// `ultracode` keeps its own wording because it means xhigh plus workflow
/// orchestration, not plain xhigh. An unrecognized tier renders muted rather
/// than vanishing, so a build that grows a level still shows it.
pub fn effort_chip(label: &str, truecolor: bool) -> Option<String> {
    let mut text = label.trim().to_lowercase();
    let (token, bold) = match text.as_str() {
        "" => return None,
        "none" => (EFFORT_NONE, false),
        "low" => (EFFORT_LOW, false),
        "medium" => (EFFORT_MEDIUM, false),
        "med" => {
            // A pure alias, so canonicalize the wording too.
            text = "medium".to_string();
            (EFFORT_MEDIUM, false)
        }
        "high" => (EFFORT_HIGH, false),
        "xhigh" | "ultracode" => (EFFORT_MAX, false),
        "max" => (EFFORT_MAX, true),
        _ => (EFFORT_NONE, false),
    };
    Some(if bold {
        token.bold(&text, truecolor)
    } else {
        token.paint(&text, truecolor)
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// HYPERLINKS -- OSC 8, and the width accounting that has to understand it
// ═══════════════════════════════════════════════════════════════════════════════

/// Wrap `text` in an OSC 8 hyperlink.
///
/// Claude Code parses the sequence out of statusline output and re-emits it as a
/// real hyperlink on terminals that support one, falling back to plain text
/// elsewhere, so there is no terminal to detect here. The BEL terminator is the
/// widely supported form and the one Claude Code emits itself.
///
/// A URL carrying a control character would terminate the sequence early and
/// spill the remainder onto the line, so those are returned unlinked.
pub fn hyperlink(text: &str, url: &str) -> String {
    if url.is_empty() || url.chars().any(char::is_control) {
        return text.to_string();
    }
    format!("\u{1b}]8;;{url}\u{7}{text}\u{1b}]8;;\u{7}")
}

/// Drop ANSI escapes, leaving the characters a terminal actually shows.
///
/// Handles both CSI (`ESC [ ... final`) and OSC (`ESC ] ... BEL` or `ST`); the
/// OSC arm is what keeps a hyperlink's URL out of the width budget.
pub fn strip_ansi(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            stripped.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC runs to BEL or to ST (ESC \).
                while let Some(code) = chars.next() {
                    if code == '\u{7}' {
                        break;
                    }
                    if code == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {
                stripped.push(ch);
            }
        }
    }

    stripped
}

/// Columns a terminal spends on `text`, ignoring escape sequences.
///
/// Measured in display columns rather than `char`s because Claude Code lays the
/// footer and the agent panel out with a real width function: counting chars
/// under-measures CJK (one char, two columns) and over-measures emoji sequences
/// (several chars, two columns). Both inputs are outside our control -- agent
/// names and the model-written task labels in the panel -- and a mismatch either
/// truncates the tail or drops a segment early.
pub fn visible_width(text: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(text).as_str())
}

/// Truncate `text` to `max_width` display columns, marking the cut with `…`.
///
/// Operates on unstyled text: every caller shortens first and paints after, so a
/// cut can never land inside an escape sequence. Wide characters are dropped
/// whole rather than split, which can leave the result one column short of the
/// budget -- the alternative is overflowing it.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if visible_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    // The ellipsis itself needs a column.
    let budget = max_width - 1;
    let mut out = String::with_capacity(text.len());
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Interpolate between semantic gradient colors: green -> yellow -> red.
/// Identical to the former `color_scale_rgb` in display.rs.
fn color_scale_rgb(value: f64, max: f64) -> (u8, u8, u8) {
    let ratio = (value / max).clamp(0.0, 1.0);

    if ratio < 0.5 {
        // Green to Yellow
        let t = ratio * 2.0;
        let r = (GRADIENT_LOW.0 as f64 + (GRADIENT_MID.0 as f64 - GRADIENT_LOW.0 as f64) * t) as u8;
        let g = (GRADIENT_LOW.1 as f64 + (GRADIENT_MID.1 as f64 - GRADIENT_LOW.1 as f64) * t) as u8;
        let b = (GRADIENT_LOW.2 as f64 + (GRADIENT_MID.2 as f64 - GRADIENT_LOW.2 as f64) * t) as u8;
        (r, g, b)
    } else {
        // Yellow to Red
        let t = (ratio - 0.5) * 2.0;
        let r =
            (GRADIENT_MID.0 as f64 + (GRADIENT_HIGH.0 as f64 - GRADIENT_MID.0 as f64) * t) as u8;
        let g =
            (GRADIENT_MID.1 as f64 + (GRADIENT_HIGH.1 as f64 - GRADIENT_MID.1 as f64) * t) as u8;
        let b =
            (GRADIENT_MID.2 as f64 + (GRADIENT_HIGH.2 as f64 - GRADIENT_MID.2 as f64) * t) as u8;
        (r, g, b)
    }
}

/// Build a dynamic `ColorToken` from a value/max ratio with smooth RGB
/// interpolation for truecolor and stepped green/yellow/red for ANSI.
pub fn gradient(value: f64, max: f64) -> ColorToken {
    let (r, g, b) = color_scale_rgb(value, max);
    let normalized = (value / max).clamp(0.0, 1.0);
    let ansi = if normalized >= 0.66 {
        Ansi::Red
    } else if normalized >= 0.33 {
        Ansi::Yellow
    } else {
        Ansi::Green
    };
    ColorToken::new((r, g, b), ansi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_osc_hyperlinks_with_either_terminator() {
        let bel = hyperlink("repo", "file:///a/b");
        assert_eq!(strip_ansi(&bel), "repo");
        assert_eq!(visible_width(&bel), 4);

        let st = "\u{1b}]8;;file:///a/b\u{1b}\\repo\u{1b}]8;;\u{1b}\\";
        assert_eq!(strip_ansi(st), "repo");
        assert_eq!(visible_width(st), 4);
    }

    #[test]
    fn strips_sgr_nested_inside_a_hyperlink() {
        let styled = hyperlink(&ACCENT.paint("repo", true), "file:///a/b");
        assert_eq!(strip_ansi(&styled), "repo");
        assert_eq!(visible_width(&styled), 4);
    }

    #[test]
    fn refuses_to_link_a_url_holding_a_control_character() {
        // A stray BEL or ESC would close the sequence early and spill the rest
        // of the URL onto the status line as visible text.
        assert_eq!(hyperlink("repo", "file:///a\u{7}b"), "repo");
        assert_eq!(hyperlink("repo", "file:///a\u{1b}b"), "repo");
        assert_eq!(hyperlink("repo", "file:///a\nb"), "repo");
        assert_eq!(hyperlink("repo", ""), "repo");
    }

    #[test]
    fn measures_display_columns_not_chars() {
        // CJK is one char but two columns; counting chars under-measures and
        // Claude Code then ellipsizes the tail we thought would fit.
        assert_eq!("日本語".chars().count(), 3);
        assert_eq!(visible_width("日本語"), 6);

        // A ZWJ emoji sequence is several chars but two columns; counting chars
        // over-measures and drops a segment that would have fit.
        let family = "\u{1F468}\u{200D}\u{1F4BB}";
        assert!(family.chars().count() > 2);
        assert_eq!(visible_width(family), 2);

        // Combining marks add no columns of their own.
        assert_eq!(visible_width("e\u{301}"), 1);
    }

    #[test]
    fn truncates_on_columns_and_never_splits_a_wide_char() {
        // Budget 5 leaves 4 columns before the ellipsis, so two wide chars fit.
        assert_eq!(truncate_to_width("日本語です", 5), "日本…");
        assert!(visible_width(&truncate_to_width("日本語です", 5)) <= 5);

        // An odd budget cannot be filled exactly by wide chars; coming up one
        // column short beats overflowing.
        let odd = truncate_to_width("日本語です", 4);
        assert_eq!(odd, "日…");
        assert!(visible_width(&odd) <= 4);

        assert_eq!(truncate_to_width("hello world", 8), "hello w…");
        assert_eq!(truncate_to_width("short", 10), "short");
        assert_eq!(truncate_to_width("anything", 0), "");
        assert_eq!(truncate_to_width("anything", 1), "…");
    }

    #[test]
    fn measures_a_styled_and_linked_string_by_its_visible_text() {
        let decorated = hyperlink(&ACCENT.paint("日本語", true), "file:///a/b");
        assert_eq!(visible_width(&decorated), 6);
    }

    #[test]
    fn maps_every_effort_tier_claude_code_can_send() {
        for label in ["none", "low", "medium", "high", "xhigh", "max"] {
            let chip = effort_chip(label, false).unwrap_or_else(|| panic!("no chip for {label}"));
            assert_eq!(strip_ansi(&chip), label);
        }
    }

    #[test]
    fn accepts_the_effort_aliases_claude_code_accepts() {
        // `med` and `ultracode` are valid CLAUDE_CODE_EFFORT_LEVEL values, and
        // ultracode keeps its own wording because it is not plain xhigh.
        assert_eq!(strip_ansi(&effort_chip("med", false).unwrap()), "medium");
        assert_eq!(
            strip_ansi(&effort_chip("ULTRACODE", false).unwrap()),
            "ultracode"
        );
        assert_eq!(
            effort_chip("ultracode", true).unwrap(),
            EFFORT_MAX.paint("ultracode", true)
        );
    }

    #[test]
    fn renders_an_unknown_tier_muted_rather_than_dropping_it() {
        // A build that grows a level should still show it; only an empty label
        // means "no chip".
        let chip = effort_chip("turbo", true).expect("unknown tier still renders");
        assert_eq!(strip_ansi(&chip), "turbo");
        assert_eq!(chip, EFFORT_NONE.paint("turbo", true));
        assert!(effort_chip("   ", false).is_none());
    }

    #[test]
    fn bolds_only_the_max_tier() {
        assert_eq!(
            effort_chip("max", true).unwrap(),
            EFFORT_MAX.bold("max", true)
        );
        assert_eq!(
            effort_chip("xhigh", true).unwrap(),
            EFFORT_MAX.paint("xhigh", true)
        );
    }
}
