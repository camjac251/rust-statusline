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

impl ColorToken {
    pub const fn new(rgb: (u8, u8, u8), ansi: Ansi) -> Self {
        Self { rgb, ansi }
    }

    /// Apply color to text. Truecolor when `tc` is true, ANSI fallback otherwise.
    #[cfg(feature = "colors")]
    pub fn paint(&self, text: &str, tc: bool) -> String {
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
pub const MODEL_OPUS: ColorToken = ColorToken::new((200, 160, 255), Ansi::BrightMagenta);
pub const MODEL_SONNET: ColorToken = ColorToken::new((255, 200, 100), Ansi::BrightYellow);
pub const MODEL_HAIKU: ColorToken = ColorToken::new((100, 220, 255), Ansi::BrightCyan);

// -- Semantic -----------------------------------------------------------------
pub const SUCCESS: ColorToken = ColorToken::new((134, 239, 172), Ansi::Green);
pub const WARNING: ColorToken = ColorToken::new((253, 224, 71), Ansi::Yellow);
pub const ERROR: ColorToken = ColorToken::new((248, 113, 113), Ansi::Red);
pub const MUTED: ColorToken = ColorToken::new((148, 163, 184), Ansi::BrightBlack);
pub const ACCENT: ColorToken = ColorToken::new((96, 165, 250), Ansi::BrightBlue);

// -- Effort (heat gradient) ---------------------------------------------------
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
