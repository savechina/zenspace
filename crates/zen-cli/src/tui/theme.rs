use ratatui::style::{Color, Modifier, Style};
use ratatui_themes::{ThemeName, ThemePalette};

/// Color layer architecture for theme system.
///
/// Deep Ocean 5-layer model:
/// - Layer 0 (Canvas): Terminal bg - physical container (RGB 9,13,22)
/// - Layer 1 (Deep Base): Container borders/shadows - structural elements (RGB 6,106,143)
/// - Layer 2 (Focus Static): Focused content/links - primary attention (RGB 43,160,152)
/// - Layer 3 (Active): Dynamic elements/cursors - energy flow (RGB 0,162,97)
/// - Layer 4 (Muted): Secondary text/logs - ambient info (RGB 30,80,85)
pub trait OutputTheme {
    /// Layer 2: Heading hierarchy (level 1-6, higher = more prominent)
    fn heading(&self, level: u8) -> Style;

    /// Layer 2: Bold emphasis for key content
    fn bold(&self) -> Style;

    /// Layer 2: Italic for annotations/citations
    fn italic(&self) -> Style;

    /// Layer 2: Inline code spans
    fn code_inline(&self) -> Style;

    /// Layer 1: Code block border framing
    fn code_block_border(&self) -> Style;

    /// Layer 2: Code block language label
    fn code_block_lang(&self) -> Style;

    /// Layer 1: Table cell borders
    fn table_border(&self) -> Style;

    /// Layer 2: Table header row (bold background highlight)
    fn table_header(&self) -> Style;

    /// Layer 2: Blockquote vertical rule
    fn blockquote(&self) -> Style;

    /// Layer 1: List bullet markers
    fn list_bullet(&self) -> Style;

    /// Layer 2: Hyperlinks (underlined)
    fn link(&self) -> Style;

    /// Layer 3: Error text (typically red/warm colors, high visibility)
    fn error(&self) -> Style;

    /// Layer 3: Streaming cursor indicator (blinking/animated)
    fn streaming_cursor(&self) -> Style;

    /// Layer 0: Terminal/container background. `Color::Reset` defers to emulator default.
    fn bg(&self) -> Color {
        Color::Reset
    }

    /// Layer 2: Banner gradient light endpoint (top of gradient)
    fn zen_core_light(&self) -> Color;

    /// Layer 2: Banner gradient dark endpoint (bottom of gradient)
    fn zen_core_dark(&self) -> Color;

    /// Layer 1: Shadow/projection color (typically darker than bg)
    fn shadow(&self) -> Color;

    /// Layer 4: Dimmed auxiliary text (status labels, diagnostic prefixes, streaming buffers)
    fn text_muted(&self) -> Style;

    /// Layer 2: Info accent color (tooltips, info boxes)
    fn info_accent(&self) -> Color;

    /// User message background: 12% white blend on theme bg
    fn user_bg(&self) -> Color {
        blend_toward_white(self.bg(), 12)
    }

    /// User message prefix glyph style (dim + bold)
    fn user_prefix(&self) -> Style {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::DIM)
    }

    /// Agent message prefix glyph style (dim bullet)
    fn agent_prefix(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Turn separator style — dim + accent color for visual distinction
    fn separator(&self) -> Style {
        Style::default()
            .fg(self.info_accent())
            .add_modifier(Modifier::DIM)
    }

    /// Text selection background color (highlighted region)
    fn selection_bg(&self) -> Color {
        self.info_accent()
    }

    /// Text selection foreground color (highlighted region)
    fn selection_fg(&self) -> Color {
        Color::Black
    }
}

fn blend_toward_white(color: Color, pct: u8) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            let p = pct as u16;
            Color::Rgb(
                (r as u16 + ((255 - r as u16) * p / 100)) as u8,
                (g as u16 + ((255 - g as u16) * p / 100)) as u8,
                (b as u16 + ((255 - b as u16) * p / 100)) as u8,
            )
        }
        _ => Color::Rgb(30, 30, 30),
    }
}

// ---------------------------------------------------------------------------
// Ratatui Themes Bridge — adapts ratatui-themes ThemePalette to OutputTheme
// Maps semantic palette fields to our 24-method OutputTheme trait.
// ---------------------------------------------------------------------------

pub struct RatatuiThemesBridge {
    palette: ThemePalette,
}

impl RatatuiThemesBridge {
    pub fn from_theme_name(name: ThemeName) -> Self {
        Self {
            palette: name.palette(),
        }
    }
}

impl OutputTheme for RatatuiThemesBridge {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(self.palette.accent)
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(self.palette.accent)
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(self.palette.secondary)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(self.palette.fg)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(self.palette.fg)
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(self.palette.muted)
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(self.palette.accent)
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(self.palette.muted)
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(self.palette.secondary)
    }

    fn table_border(&self) -> Style {
        Style::default().fg(self.palette.muted)
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(self.palette.fg)
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(self.palette.success)
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(self.palette.secondary)
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(self.palette.accent)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(self.palette.error)
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(self.palette.selection)
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        self.palette.bg
    }

    fn zen_core_light(&self) -> Color {
        self.palette.accent
    }

    fn zen_core_dark(&self) -> Color {
        self.palette.secondary
    }

    fn shadow(&self) -> Color {
        self.palette.muted
    }

    fn text_muted(&self) -> Style {
        Style::default().fg(self.palette.muted)
    }

    fn info_accent(&self) -> Color {
        self.palette.accent
    }
}

pub fn from_name(name: &str) -> Box<dyn OutputTheme> {
    match name {
        "classic" => Box::new(ClassicTheme),
        "catppuccin" => Box::new(CatppuccinMochaTheme),
        "deep-ocean" => Box::new(DeepOceanTheme),
        "cyber-purple" => Box::new(CyberPurpleTheme),
        "eink" => Box::new(EinkTheme),
        // ratatui-themes names
        "dracula" => Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::Dracula)),
        "one-dark-pro" | "onedarkpro" => {
            Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::OneDarkPro))
        }
        "nord" => Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::Nord)),
        "catppuccin-mocha" => Box::new(RatatuiThemesBridge::from_theme_name(
            ThemeName::CatppuccinMocha,
        )),
        "catppuccin-latte" => Box::new(RatatuiThemesBridge::from_theme_name(
            ThemeName::CatppuccinLatte,
        )),
        "gruvbox-dark" | "gruvbox" => {
            Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::GruvboxDark))
        }
        "gruvbox-light" => Box::new(RatatuiThemesBridge::from_theme_name(
            ThemeName::GruvboxLight,
        )),
        "tokyo-night" | "tokyonight" => {
            Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::TokyoNight))
        }
        "solarized-dark" => Box::new(RatatuiThemesBridge::from_theme_name(
            ThemeName::SolarizedDark,
        )),
        "solarized-light" => Box::new(RatatuiThemesBridge::from_theme_name(
            ThemeName::SolarizedLight,
        )),
        "monokai-pro" | "monokai" => {
            Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::MonokaiPro))
        }
        "rose-pine" | "rosepine" => {
            Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::RosePine))
        }
        "kanagawa" => Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::Kanagawa)),
        "everforest" => Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::Everforest)),
        "cyberpunk" => Box::new(RatatuiThemesBridge::from_theme_name(ThemeName::Cyberpunk)),
        _ => Box::new(ZenTheme),
    }
}

pub fn supports_truecolor() -> bool {
    match std::env::var("COLORTERM") {
        Ok(v) => v == "truecolor" || v == "24bit",
        Err(_) => match std::env::var("TERM") {
            Ok(v) => v.contains("256") || v.contains("color"),
            Err(_) => false,
        },
    }
}

pub fn auto_select() -> Box<dyn OutputTheme> {
    if !supports_truecolor() {
        return Box::new(EinkTheme);
    }
    Box::new(ZenTheme)
}

pub fn no_color() -> Box<dyn OutputTheme> {
    Box::new(NoColorTheme)
}

// ---------------------------------------------------------------------------
// Zen — Wabi-sabi natural palette (DEFAULT)
// Inspired by Japanese temple aesthetics: moss, sand, wood, stone.
// Warm, organic, calm. Designed for zen/flow state immersion.
// bg: warm charcoal (18,20,17)
// banner: moss green gradient
// accent: sand/amber
// ---------------------------------------------------------------------------

pub struct ZenTheme;

impl OutputTheme for ZenTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Rgb(178, 130, 100)) // terracotta copper
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Rgb(110, 170, 120)) // sage green
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Rgb(196, 178, 136)) // sand/amber
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Rgb(210, 205, 196)) // warm white
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(210, 205, 196)) // warm white
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(145, 138, 124)) // warm stone gray
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Rgb(110, 170, 120)) // sage green
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::Rgb(80, 78, 72)) // dark warm gray
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Rgb(196, 178, 136)) // sand
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::Rgb(80, 78, 72)) // dark warm gray
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(210, 205, 196)) // warm white
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Rgb(110, 170, 120)) // sage green
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(Color::Rgb(145, 138, 124)) // warm stone
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(90, 143, 110)) // moss green
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Rgb(196, 112, 102)) // warm terracotta red
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(196, 178, 136)) // sand yellow
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        Color::Rgb(18, 20, 17) // warm charcoal (aged wood)
    }
    fn zen_core_light(&self) -> Color {
        Color::Rgb(90, 143, 110) // moss green (苔 koke)
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(40, 100, 72) // dark moss
    }
    fn shadow(&self) -> Color {
        Color::Rgb(26, 82, 60) // deep forest (自然投影)
    }
    fn text_muted(&self) -> Style {
        Style::default().fg(Color::Rgb(145, 138, 124)) // warm stone gray (砂利)
    }
    fn info_accent(&self) -> Color {
        Color::Rgb(196, 178, 136) // sand/amber (砂 suna)
    }
}

impl Default for ZenTheme {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Classic Theme — preserves prior banner teal + classic Cyan/Yellow conventions.
// ---------------------------------------------------------------------------

pub struct ClassicTheme;

impl OutputTheme for ClassicTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Cyan)
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Green)
    }

    fn list_bullet(&self) -> Style {
        Style::default()
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Red)
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    }

    fn zen_core_light(&self) -> Color {
        Color::Rgb(64, 144, 172)
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(6, 106, 143)
    }
    fn shadow(&self) -> Color {
        Color::Indexed(236)
    }
    fn text_muted(&self) -> Style {
        // Bright gray — readable on dark and light terminal bgs, visually secondary.
        Style::default().fg(Color::Indexed(250))
    }
    fn info_accent(&self) -> Color {
        // Punchy mint-teal — high contrast on dark bg, not neon, complements teal banner.
        Color::Rgb(80, 220, 190)
    }
}

impl Default for ClassicTheme {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Catppuccin Mocha (https://github.com/catppuccin/catppuccin)
// ---------------------------------------------------------------------------

pub struct CatppuccinMochaTheme;

impl OutputTheme for CatppuccinMochaTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Rgb(203, 166, 247)) // mauve
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Rgb(137, 180, 250)) // blue
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Rgb(249, 226, 175)) // yellow
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Rgb(205, 214, 244)) // text
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(205, 214, 244)) // text
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(166, 157, 187)) // overlay2
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Rgb(245, 189, 183)) // peach
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::Rgb(108, 112, 134)) // overlay1
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Rgb(166, 157, 187)) // overlay2
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::Rgb(92, 96, 119)) // surface2
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(217, 206, 244)) // lavender
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Rgb(166, 227, 161)) // green
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(Color::Rgb(245, 189, 183)) // peach
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(137, 221, 255)) // sky
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Rgb(243, 139, 168)) // red (catppuccin-style)
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(249, 226, 175)) // yellow
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        Color::Rgb(30, 30, 46) // mantle
    }
    fn zen_core_light(&self) -> Color {
        Color::Rgb(166, 227, 161) // green
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(88, 150, 95)
    }
    fn shadow(&self) -> Color {
        Color::Rgb(88, 91, 112) // surface1
    }
    fn text_muted(&self) -> Style {
        Style::default().fg(Color::Rgb(108, 112, 134)) // overlay1
    }
    fn info_accent(&self) -> Color {
        Color::Rgb(137, 180, 250) // blue
    }
}

impl Default for CatppuccinMochaTheme {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Flow 方案一 — 深海静谧 (心流心流体感)
// bg: Slate-900 (15,23,42)
// accent: 薄荷绿 / 冰川蓝
// ---------------------------------------------------------------------------

pub struct DeepOceanTheme;

impl OutputTheme for DeepOceanTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Rgb(43, 160, 152)) // 极光青蓝 (aurora cyan-blue)
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Rgb(0, 162, 97)) // 翡翠薄荷绿 (mint emerald)
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Rgb(100, 116, 139)) // slate-400
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Rgb(203, 213, 225)) // slate-300
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(203, 213, 225)) // slate-300
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(148, 163, 184)) // slate-400
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Rgb(43, 160, 152)) // 极光青蓝
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::Rgb(6, 106, 143)) // Layer 1: 深海钢青蓝
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Rgb(100, 116, 139))
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::Rgb(6, 106, 143)) // Layer 1: 深海钢青蓝
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(203, 213, 225)) // slate-300
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Rgb(0, 162, 97)) // 翡翠薄荷绿
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(Color::Rgb(43, 160, 152)) // 极光青蓝
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(43, 160, 152)) // 极光青蓝
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Rgb(248, 113, 113)) // red-400, softened for low-light
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(0, 162, 97)) // 翡翠薄荷绿 — 编译通过/success
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        Color::Rgb(9, 13, 22) // 极深海沟蓝 (deep-ocean trench)
    }
    fn zen_core_light(&self) -> Color {
        Color::Rgb(43, 160, 152) // 极光青蓝 — flowing water / rational
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(43, 160, 152) // solid: matches light so lerp produces uniform █ color
    }
    fn shadow(&self) -> Color {
        Color::Rgb(0, 162, 97) // 翡翠薄荷绿 — 3D projection (vitality / success)
    }
    fn text_muted(&self) -> Style {
        Style::default().fg(Color::Rgb(30, 80, 85)) // Layer 4: 次要背景流
    }
    fn info_accent(&self) -> Color {
        Color::Rgb(6, 106, 143) // Layer 1: 深海钢青蓝
    }
}

impl Default for DeepOceanTheme {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Flow 方案二 — 数字禅院 (极客紫)
// bg: Zinc-900 (24,24,27)
// accent: 女巫紫 / 浅薰衣草紫
// ---------------------------------------------------------------------------

pub struct CyberPurpleTheme;

impl OutputTheme for CyberPurpleTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Rgb(192, 132, 252)) // purple-400
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Rgb(155, 89, 182))
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Rgb(113, 113, 122)) // zinc-500
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Rgb(212, 212, 216)) // zinc-300
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(212, 212, 216))
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(161, 161, 170)) // zinc-400
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Rgb(192, 132, 252))
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::Indexed(235))
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Rgb(113, 113, 122))
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::Rgb(63, 63, 70)) // zinc-800
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(212, 212, 216))
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Rgb(155, 89, 182))
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(Color::Rgb(192, 132, 252))
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(192, 132, 252))
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Rgb(244, 114, 182)) // pink-400 to avoid purple clash
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(192, 132, 252))
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        Color::Rgb(24, 24, 27) // Tailwind zinc-900
    }
    fn zen_core_light(&self) -> Color {
        Color::Rgb(175, 129, 202)
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(102, 48, 133)
    }
    fn shadow(&self) -> Color {
        Color::Rgb(60, 40, 75) // dark purple — natural shadow of banner purple
    }
    fn text_muted(&self) -> Style {
        Style::default().fg(Color::Rgb(130, 130, 140)) // zinc-500, brightened for WCAG AA
    }
    fn info_accent(&self) -> Color {
        Color::Rgb(192, 132, 252) // purple-400
    }
}

impl Default for CyberPurpleTheme {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Flow 方案三 — 极致极简 (E-Ink Matrix, monochrome)
// bg: Zinc-950 (9,9,11)
// accent: Zinc-200 (228,228,231) — NOT pure white
// ---------------------------------------------------------------------------

pub struct EinkTheme;

impl OutputTheme for EinkTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::Rgb(228, 228, 231)) // zinc-200
                .add_modifier(Modifier::BOLD),
            2 => Style::default()
                .fg(Color::Rgb(161, 161, 170)) // zinc-400
                .add_modifier(Modifier::BOLD),
            3 => Style::default()
                .fg(Color::Rgb(113, 113, 122)) // zinc-500
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Rgb(228, 228, 231))
                .add_modifier(Modifier::BOLD),
        }
    }

    fn bold(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(228, 228, 231))
            .add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(161, 161, 170))
            .add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().fg(Color::Rgb(228, 228, 231))
    }

    fn code_block_border(&self) -> Style {
        Style::default().fg(Color::Indexed(238))
    }

    fn code_block_lang(&self) -> Style {
        Style::default().fg(Color::Rgb(161, 161, 170))
    }

    fn table_border(&self) -> Style {
        Style::default().fg(Color::Rgb(63, 63, 70)) // zinc-800
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(228, 228, 231))
            .add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Rgb(161, 161, 170))
    }

    fn list_bullet(&self) -> Style {
        Style::default().fg(Color::Rgb(161, 161, 170))
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(228, 228, 231))
            .add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().fg(Color::Rgb(252, 165, 165)) // red-300 monochrome-compatible
    }

    fn streaming_cursor(&self) -> Style {
        Style::default()
            .fg(Color::Rgb(228, 228, 231))
            .add_modifier(Modifier::BOLD)
    }

    fn bg(&self) -> Color {
        Color::Rgb(9, 9, 11) // Tailwind zinc-950
    }
    fn zen_core_light(&self) -> Color {
        Color::Rgb(228, 228, 231)
    }
    fn zen_core_dark(&self) -> Color {
        Color::Rgb(141, 141, 151) // zinc-400
    }
    fn shadow(&self) -> Color {
        Color::Indexed(238)
    }
    fn text_muted(&self) -> Style {
        Style::default().fg(Color::Rgb(135, 135, 145)) // zinc-500, brightened for WCAG AA
    }
    fn info_accent(&self) -> Color {
        Color::Rgb(228, 228, 231) // monochrome — same as zen_core_light
    }
}

impl Default for EinkTheme {
    fn default() -> Self {
        Self
    }
}

pub struct NoColorTheme;

impl OutputTheme for NoColorTheme {
    fn heading(&self, level: u8) -> Style {
        let modifier = if level <= 2 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        Style::default().add_modifier(modifier)
    }

    fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn italic(&self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    fn code_inline(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn code_block_border(&self) -> Style {
        Style::default()
    }

    fn code_block_lang(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> Style {
        Style::default()
    }

    fn table_header(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn blockquote(&self) -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    fn list_bullet(&self) -> Style {
        Style::default()
    }

    fn link(&self) -> Style {
        Style::default().add_modifier(Modifier::UNDERLINED)
    }

    fn error(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn streaming_cursor(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    fn zen_core_light(&self) -> Color {
        Color::Reset
    }

    fn zen_core_dark(&self) -> Color {
        Color::Reset
    }

    fn shadow(&self) -> Color {
        Color::Reset
    }

    fn text_muted(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    fn info_accent(&self) -> Color {
        Color::Reset
    }
}

impl Default for NoColorTheme {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_themes() -> Vec<(&'static str, Box<dyn OutputTheme>)> {
        vec![
            ("zen", from_name("zen")),
            ("classic", from_name("classic")),
            ("catppuccin", from_name("catppuccin")),
            ("deep-ocean", from_name("deep-ocean")),
            ("cyber-purple", from_name("cyber-purple")),
            ("eink", from_name("eink")),
        ]
    }

    #[test]
    fn test_from_name_factory_routes() {
        // Non-existent name → ZenTheme
        let _ = from_name("unknown-theme");
        for (name, _) in all_themes() {
            let _ = from_name(name);
        }
    }

    #[test]
    fn test_all_themes_implement_new_methods() {
        for (_name, theme) in all_themes() {
            let _ = theme.bg();
            let _ = theme.zen_core_light();
            let _ = theme.zen_core_dark();
            let _ = theme.shadow();
            let _ = theme.text_muted();
            let _ = theme.info_accent();
            let _ = theme.user_bg();
            let _ = theme.user_prefix();
            let _ = theme.agent_prefix();
            let _ = theme.separator();
            let _ = theme.selection_bg();
            let _ = theme.selection_fg();
        }
    }

    #[test]
    fn test_eink_rejects_pure_white() {
        let theme = EinkTheme;
        match theme.zen_core_light() {
            Color::Rgb(r, g, b) => {
                assert!(
                    r < 255 && g < 255 && b < 255,
                    "E-Ink must not use pure white"
                );
                assert_eq!((r, g, b), (228, 228, 231));
            }
            other => panic!("expected Rgb, got {other:?}"),
        }
    }

    #[test]
    fn test_classic_preserves_teal_banner() {
        let theme = ClassicTheme;
        assert_eq!(theme.zen_core_light(), Color::Rgb(64, 144, 172));
        assert_eq!(theme.zen_core_dark(), Color::Rgb(6, 106, 143));
        assert_eq!(theme.shadow(), Color::Indexed(236));
    }

    #[test]
    fn test_deep_ocean_palette_matches_spec() {
        let theme = DeepOceanTheme;
        assert_eq!(theme.bg(), Color::Rgb(9, 13, 22));
        assert_eq!(theme.info_accent(), Color::Rgb(6, 106, 143)); // Layer 1: deep steel blue
        assert_eq!(theme.zen_core_light(), theme.zen_core_dark()); // solid banner
        assert_eq!(theme.shadow(), Color::Rgb(0, 162, 97));
        // Layer 4 muted: low-contrast ambient text
        let muted_fg = theme.text_muted().fg.unwrap_or(Color::Reset);
        assert_eq!(muted_fg, Color::Rgb(30, 80, 85));
    }

    #[test]
    fn test_zen_natural_palette() {
        let theme = ZenTheme;
        assert_eq!(theme.bg(), Color::Rgb(18, 20, 17));
        assert_eq!(theme.zen_core_light(), Color::Rgb(90, 143, 110));
        assert_eq!(theme.zen_core_dark(), Color::Rgb(40, 100, 72));
        assert_ne!(theme.zen_core_light(), theme.zen_core_dark());
        assert_eq!(theme.shadow(), Color::Rgb(26, 82, 60));
        assert_eq!(theme.info_accent(), Color::Rgb(196, 178, 136));
        assert_ne!(theme.zen_core_light(), theme.info_accent());
    }
}
