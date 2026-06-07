use crate::tui::theme::OutputTheme;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone)]
pub struct BannerCell {
    pub text: String,
    pub zen_core_light: Color,
    pub zen_core_dark: Color,
    pub shadow: Color,
}

impl BannerCell {
    pub fn new(text: impl Into<String>, theme: &dyn OutputTheme) -> Self {
        Self {
            text: text.into(),
            zen_core_light: theme.zen_core_light(),
            zen_core_dark: theme.zen_core_dark(),
            shadow: theme.shadow(),
        }
    }

    pub fn display_lines(&self) -> Vec<Line<'static>> {
        let rows: Vec<&str> = self.text.lines().collect();
        let total = rows.len();
        if total == 0 {
            return Vec::new();
        }

        let light = rgb_or_default(self.zen_core_light);
        let dark = rgb_or_default(self.zen_core_dark);
        let body_style_for = |row_idx: usize| {
            let frac = if total <= 1 {
                0.0
            } else {
                (row_idx as f64) / (total - 1) as f64
            };
            let (r, g, b) = lerp_rgb(light, dark, frac.clamp(0.0, 1.0));
            Style::default().fg(Color::Rgb(r, g, b))
        };
        let shadow_style = Style::default().fg(self.shadow);
        let filler_style = Style::default().fg(Color::Indexed(234));

        rows.into_iter()
            .enumerate()
            .map(|(row_idx, line)| {
                let body_style = body_style_for(row_idx);

                let mut current_style = body_style;
                let mut current_run = String::new();
                let mut spans: Vec<Span<'static>> = Vec::new();

                for ch in line.chars() {
                    let (symbol, style) = match ch {
                        '█' => ("█", body_style),
                        '▒' => ("▒", shadow_style),
                        '_' => (" ", filler_style),
                        other_ch => {
                            let mut buf = [0u8; 4];
                            let s = other_ch.encode_utf8(&mut buf);
                            if !current_run.is_empty() {
                                spans.push(Span::styled(
                                    std::mem::take(&mut current_run),
                                    current_style,
                                ));
                            }
                            spans.push(Span::styled(s.to_string(), body_style));
                            current_style = body_style;
                            continue;
                        }
                    };

                    if !current_run.is_empty() && current_style != style {
                        spans.push(Span::styled(
                            std::mem::take(&mut current_run),
                            current_style,
                        ));
                        current_style = style;
                    } else {
                        current_style = style;
                    }
                    current_run.push_str(symbol);
                }

                if !current_run.is_empty() {
                    spans.push(Span::styled(current_run, current_style));
                }

                Line::from(spans)
            })
            .collect()
    }
}

fn rgb_or_default(c: Color) -> (f64, f64, f64) {
    match c {
        Color::Rgb(r, g, b) => (r as f64, g as f64, b as f64),
        _ => (64.0, 144.0, 172.0),
    }
}

fn lerp_rgb(a: (f64, f64, f64), b: (f64, f64, f64), t: f64) -> (u8, u8, u8) {
    (
        (a.0 + (b.0 - a.0) * t).round() as u8,
        (a.1 + (b.1 - a.1) * t).round() as u8,
        (a.2 + (b.2 - a.2) * t).round() as u8,
    )
}

impl From<String> for BannerCell {
    fn from(s: String) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}

impl From<&str> for BannerCell {
    fn from(s: &str) -> Self {
        use crate::tui::theme::ZenTheme;
        Self::new(s, &ZenTheme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{ClassicTheme, DeepOceanTheme, EinkTheme, ZenTheme};

    #[test]
    fn test_classic_gradient_matches_teal_palette() {
        let theme = ClassicTheme;
        let cell = BannerCell::new("█\n█\n█\n█\n█", &theme);
        let lines = cell.display_lines();
        assert_eq!(lines.len(), 5);
        // Top row should match zen_core_light = (64,144,172)
        // Bottom row should match zen_core_dark = (6,106,143)
        assert_eq!(cell.zen_core_light, Color::Rgb(64, 144, 172));
        assert_eq!(cell.zen_core_dark, Color::Rgb(6, 106, 143));
    }

    #[test]
    fn test_deep_ocean_gradient_endpoints() {
        let theme = DeepOceanTheme;
        let cell = BannerCell::new("█", &theme);
        // DeepOcean uses solid blue (light == dark) + green shadow.
        assert_eq!(cell.zen_core_light, Color::Rgb(43, 160, 152));
        assert_eq!(cell.zen_core_dark, Color::Rgb(43, 160, 152));
        assert_eq!(cell.shadow, Color::Rgb(0, 162, 97));
    }

    #[test]
    fn test_eink_gradient_stays_in_grayscale() {
        let theme = EinkTheme;
        let cell = BannerCell::new("█", &theme);
        match cell.zen_core_light {
            Color::Rgb(r, g, b) => {
                // Verify: perceptual grayscale (channels within ±5) — Zinc-200 has Tailwind's
                // subtle blue tint, so we allow a small delta rather than exact equality.
                assert!((r as i16 - g as i16).abs() <= 5);
                assert!((g as i16 - b as i16).abs() <= 5);
            }
            other => panic!("expected Rgb, got {other:?}"),
        }
    }

    #[test]
    fn test_single_row_no_crash() {
        let theme = ZenTheme;
        let cell = BannerCell::new("█", &theme);
        assert_eq!(cell.display_lines().len(), 1);
    }

    #[test]
    fn test_display_lines_shadow_and_filler() {
        let theme = ZenTheme;
        let cell = BannerCell::new("█▒_", &theme);
        let lines = cell.display_lines();
        assert_eq!(lines.len(), 1);
        // Verify: "█" (body) + "▒" (shadow rendered as ▒) + "_" (filler → space) = width 3
        assert_eq!(lines[0].width(), 3);
    }

    #[test]
    fn test_empty_banner() {
        let theme = ZenTheme;
        let cell = BannerCell::new("", &theme);
        assert!(cell.display_lines().is_empty());
    }
}
