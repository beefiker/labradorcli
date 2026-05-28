use std::io;

/// Store user preferences in the Windows Registry.
/// Modeled after https://github.com/neovide/neovide/blob/main/src/windows_utils.rs .
use super::UserPreferences;
use windows_registry::{Key, CURRENT_USER};
use windows_result::HRESULT;

pub struct RegistryBackedPreferences {
    app_key_path: String,
}

static LABRADOR_REGISTRY_BASE_PATH: &str = "Software\\Labrador\\";
pub const KEY_NOT_FOUND_ERR: HRESULT = HRESULT::from_win32(0x80070002);

impl RegistryBackedPreferences {
    /// Construct a separate registry path for each channel (stable, dev, local, etc.)
    pub fn new(app_name: &str) -> Self {
        Self {
            app_key_path: LABRADOR_REGISTRY_BASE_PATH.to_owned() + app_name,
        }
    }

    /// Gets the app's registry key, creating it if it does not already exist.
    fn get_labrador_registry(&self) -> Result<Key, super::Error> {
        CURRENT_USER.create(self.app_key_path.clone()).map_err(|e| {
            log::error!("unable to access app key in Windows Registry: {e:#}");
            super::Error::IoError(io::Error::from(e))
        })
    }

    fn remove_value_from_registry(registry: &Key, key: &str) -> Result<(), super::Error> {
        match registry.remove_value(key) {
            Ok(_) => Ok(()),
            // If the key doesn't exist, then treat removal of that nonexistent key as a success.
            Err(e) if e.code() == KEY_NOT_FOUND_ERR => Ok(()),
            Err(e) => Err(super::Error::from(io::Error::from(e))),
        }
    }
}

impl UserPreferences for RegistryBackedPreferences {
    fn read_value(&self, name: &str) -> Result<Option<String>, super::Error> {
        if let Ok(value) = self.get_labrador_registry()?.get_string(name) {
            return Ok(Some(value));
        }

        Ok(None)
    }

    fn write_value(&self, key: &str, value: String) -> Result<(), super::Error> {
        self.get_labrador_registry()?
            .set_string(key, value.as_str())
            .map_err(|e| super::Error::from(io::Error::from(e)))
    }

    fn remove_value(&self, key: &str) -> Result<(), super::Error> {
        Self::remove_value_from_registry(&self.get_labrador_registry()?, key)?;
        Ok(())
    }
}
