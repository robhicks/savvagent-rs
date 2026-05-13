//! Splash screen — fullscreen HUD with connect status.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, ProviderId, Region, Screen, StyledLine,
    StyledSpan, TextMods, ThemeColor,
};

/// Cached connect state forwarded from [`super::SplashPlugin`] to each new
/// screen instance so the displayed status survives open/close cycles.
#[derive(Clone)]
pub struct CachedHud {
    /// Whether a successful connect event has been received.
    pub connected: bool,
    /// The provider that connected, if known.
    pub last_provider: Option<ProviderId>,
}

/// Fullscreen splash screen displaying the startup HUD and connect status.
pub struct SplashScreen {
    /// Cached HUD state passed in at construction time.
    pub hud: Option<CachedHud>,
}

impl SplashScreen {
    /// Create a new `SplashScreen` with the given cached HUD state.
    pub fn new(cached: Option<CachedHud>) -> Self {
        Self { hud: cached }
    }
}

#[async_trait]
impl Screen for SplashScreen {
    fn id(&self) -> String {
        "splash".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut lines = vec![
            StyledLine {
                spans: vec![StyledSpan {
                    text: "Savvagent".into(),
                    fg: Some(ThemeColor::Accent),
                    bg: None,
                    modifiers: TextMods {
                        bold: true,
                        ..Default::default()
                    },
                }],
            },
            StyledLine::plain(""),
        ];
        let status = match &self.hud {
            Some(h) if h.connected => match &h.last_provider {
                Some(p) => format!("Connected to {}.", p.as_str()),
                None => "Connected.".to_string(),
            },
            _ => "Connecting\u{2026}".to_string(),
        };
        lines.push(StyledLine::plain(status));
        lines.push(StyledLine::plain(""));
        lines.push(StyledLine::plain("Press Esc to dismiss"));
        lines
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc | KeyCodePortable::Enter => Ok(vec![Effect::CloseScreen]),
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain("Esc dismiss")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    #[tokio::test]
    async fn esc_emits_close_screen() {
        let mut s = SplashScreen::new(None);
        let effs = s
            .on_key(KeyEventPortable {
                code: KeyCodePortable::Esc,
                modifiers: KeyMods::default(),
            })
            .await
            .unwrap();
        assert!(matches!(effs.first(), Some(Effect::CloseScreen)));
    }

    #[test]
    fn render_includes_dismiss_hint() {
        let s = SplashScreen::new(None);
        let lines = s.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Press Esc to dismiss"));
    }
}
