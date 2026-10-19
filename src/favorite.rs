use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteDevice {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct Favorites {
    path: PathBuf,
    file: FavoritesFile,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct FavoritesFile {
    #[serde(default)]
    servers: BTreeMap<String, FavoriteDevice>,
}

impl Favorites {
    pub fn load() -> Result<Self> {
        Self::load_from(default_path())
    }

    pub fn for_server(&self, server: &str) -> Option<&FavoriteDevice> {
        self.file.servers.get(server)
    }

    /// Set the selected device as favorite, or clear it when it was already
    /// the favorite. The favorite is scoped to the Mender server.
    pub fn toggle(
        &mut self,
        server: &str,
        device: FavoriteDevice,
    ) -> Result<Option<&FavoriteDevice>> {
        if self
            .for_server(server)
            .is_some_and(|favorite| favorite.id == device.id)
        {
            self.file.servers.remove(server);
        } else {
            self.file.servers.insert(server.to_owned(), device);
        }
        self.save()?;
        Ok(self.for_server(server))
    }

    fn load_from(path: PathBuf) -> Result<Self> {
        let file = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("invalid favorites file at {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => FavoritesFile::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read favorites from {}", path.display()));
            }
        };
        Ok(Self { path, file })
    }

    fn save(&self) -> Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let temporary = parent.join(format!(".favorites-{}.tmp", Uuid::new_v4()));
        let json = serde_json::to_vec_pretty(&self.file)?;
        fs::write(&temporary, json)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        set_private_permissions(&temporary)?;
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(error).with_context(|| format!("failed to update {}", self.path.display()));
        }
        Ok(())
    }
}

fn default_path() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("menderctl")
        .join("favorites.json")
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> FavoriteDevice {
        FavoriteDevice {
            id: id.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn favorites_are_persisted_per_server_and_can_be_cleared() {
        let directory = std::env::temp_dir().join(format!("menderctl-{}", Uuid::new_v4()));
        let path = directory.join("favorites.json");
        let mut favorites = Favorites::load_from(path.clone()).unwrap();

        favorites
            .toggle("https://one.example", device("one", "Device one"))
            .unwrap();
        favorites
            .toggle("https://two.example", device("two", "Device two"))
            .unwrap();

        let mut reloaded = Favorites::load_from(path).unwrap();
        assert_eq!(
            reloaded.for_server("https://one.example"),
            Some(&device("one", "Device one"))
        );
        assert_eq!(
            reloaded.for_server("https://two.example"),
            Some(&device("two", "Device two"))
        );

        reloaded
            .toggle("https://one.example", device("one", "Renamed"))
            .unwrap();
        assert_eq!(reloaded.for_server("https://one.example"), None);
        assert!(reloaded.for_server("https://two.example").is_some());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selecting_another_device_replaces_the_favorite() {
        let directory = std::env::temp_dir().join(format!("menderctl-{}", Uuid::new_v4()));
        let path = directory.join("favorites.json");
        let mut favorites = Favorites::load_from(path).unwrap();

        favorites
            .toggle("https://example.com", device("old", "Old"))
            .unwrap();
        favorites
            .toggle("https://example.com", device("new", "New"))
            .unwrap();

        assert_eq!(
            favorites.for_server("https://example.com"),
            Some(&device("new", "New"))
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
