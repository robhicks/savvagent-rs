//! `internal:splash` — startup HUD + parse-error rendering, exposed as a
//! Screen. The poll-loop wiring stays in main.rs in PR 3; PR 7 replaces
//! the poll with on_event(Connect) dispatch from the host.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Screen, ScreenArgs, ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::{CachedHud, SplashScreen};

/// Plugin wrapper for the startup HUD screen.
///
/// Holds a [`CachedHud`] that is updated via [`HostEvent::Connect`] and
/// passed to each new [`SplashScreen`] instance so render state persists
/// across open/close cycles.
pub struct SplashPlugin {
    /// Most recently received connect state, forwarded to new screen instances.
    pub last_render: Option<CachedHud>,
}

impl SplashPlugin {
    /// Create a new `SplashPlugin` with no cached connect state.
    pub fn new() -> Self {
        Self { last_render: None }
    }
}

impl Default for SplashPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SplashPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.screens = vec![ScreenSpec {
            id: "splash".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: "splash".into(),
            summary: "Show the splash screen".into(),
            args_hint: None,
        }];
        contributions.hooks = vec![HookKind::HostStarting, HookKind::Connect];

        Manifest {
            id: PluginId::new("internal:splash").expect("valid built-in id"),
            name: "Splash".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Startup HUD + parse-error screen".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::OpenScreen {
            id: "splash".into(),
            args: ScreenArgs::None,
        }])
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != "splash" {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        Ok(Box::new(SplashScreen::new(self.last_render.clone())))
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        match event {
            HostEvent::HostStarting => {
                // PR 7: kick off connect probe. PR 3 just records nothing.
            }
            HostEvent::Connect { provider_id } => {
                self.last_render = Some(CachedHud {
                    connected: true,
                    last_provider: Some(provider_id),
                });
            }
            _ => {}
        }
        Ok(vec![])
    }
}
