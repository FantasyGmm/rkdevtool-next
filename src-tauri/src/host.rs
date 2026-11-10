/// Sends optional desktop events; job history and MCP logs are stored by the
/// backend independently of whether a desktop window exists.
#[derive(Clone, Default)]
pub(crate) struct Host {
    #[cfg(feature = "desktop")]
    app: Option<tauri::AppHandle>,
}

impl Host {
    #[cfg(feature = "desktop")]
    pub fn desktop(app: tauri::AppHandle) -> Self {
        Self { app: Some(app) }
    }

    #[cfg(feature = "desktop")]
    pub fn app(&self) -> Option<&tauri::AppHandle> {
        self.app.as_ref()
    }

    pub fn is_headless(&self) -> bool {
        #[cfg(feature = "desktop")]
        return self.app.is_none();
        #[cfg(not(feature = "desktop"))]
        true
    }

    pub fn emit(&self, _channel: &str, _payload: impl serde::Serialize + Clone) {
        #[cfg(feature = "desktop")]
        if let Some(app) = &self.app {
            use tauri::Emitter;
            let _ = app.emit(_channel, _payload);
        }
    }
}
