//! Welcome screen content for onboarding.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::palette;

pub fn lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "codewhale",
            Style::default()
                .fg(palette::WHALE_ACCENT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("Version {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Code means two things here: the software you ship, and the law this agent works under.",
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(Span::styled(
            "Setup is short: choose the model you want to work with, let it help draft the constitution it will live under, then read and ratify.",
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(Span::styled(
            "Nothing becomes law until you confirm. Language and provider readiness are checked along the way.",
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(Span::styled(
            "Bundled defaults are valid; amend anytime with /constitution.",
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to continue.",
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(Span::styled(
            "Ctrl+C exits at any point.",
            Style::default().fg(palette::TEXT_MUTED),
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> String {
        lines()
            .into_iter()
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn welcome_copy_centers_constitution_first_setup() {
        let body = body();

        // The dual meaning of "code" opens the arc: software and law.
        assert!(body.contains("Code means two things"));
        assert!(body.contains("the law this agent works under"));
        // The arc itself: choose a model, let it draft its own law, ratify.
        assert!(body.contains("choose the model"));
        assert!(body.contains("draft the constitution it will live under"));
        assert!(body.contains("read and ratify"));
        assert!(body.contains("Nothing becomes law until you confirm"));
        assert!(body.contains("provider readiness"));
        assert!(body.contains("/constitution"));
        assert!(!body.contains("add an API key"));
        assert!(!body.contains("land in the chat"));
    }
}
