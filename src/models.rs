use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub identity_data: Value,
    #[serde(default)]
    pub created_ts: String,
    #[serde(default)]
    pub updated_ts: String,
    #[serde(default)]
    pub decommissioning: bool,
    #[serde(default)]
    pub auth_sets: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InventoryDevice {
    pub id: String,
    #[serde(default)]
    pub attributes: Vec<InventoryAttribute>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InventoryAttribute {
    #[serde(default, alias = "attribute")]
    pub name: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub timestamp: Option<String>,
}

impl InventoryDevice {
    pub fn display_name(&self) -> String {
        const PRIORITIES: &[(&str, &str)] = &[
            ("tags", "name"),
            ("inventory", "hostname"),
            ("identity", "name"),
            ("identity", "hostname"),
        ];
        for (scope, name) in PRIORITIES {
            if let Some(value) = self
                .attribute(scope, name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                return value.to_owned();
            }
        }

        self.attributes
            .iter()
            .find(|attribute| {
                attribute.timestamp.is_some()
                    && attribute
                        .value
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
            })
            .and_then(|attribute| attribute.value.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                let end = self.id.len().min(12);
                format!("device-{}", &self.id[..end])
            })
    }

    pub fn updated_at(&self) -> Option<&str> {
        self.attribute("system", "updated_ts")
            .or_else(|| self.attribute("identity", "updated_ts"))
            .and_then(Value::as_str)
            .or_else(|| {
                self.attributes
                    .iter()
                    .filter_map(|attribute| attribute.timestamp.as_deref())
                    .max()
            })
    }

    fn attribute(&self, scope: &str, name: &str) -> Option<&Value> {
        self.attributes
            .iter()
            .find(|attribute| attribute.scope == scope && attribute.name == name)
            .map(|attribute| &attribute.value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artifact {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub modified: String,
    #[serde(default)]
    pub device_types_compatible: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct DownloadLink {
    pub uri: String,
}

#[derive(Debug, Serialize)]
pub struct DeploymentRequest<'a> {
    pub name: String,
    pub artifact_name: &'a str,
    pub devices: Vec<&'a str>,
}

impl Artifact {
    /// Update type of the artifact (`rootfs-image`, `docker-compose`, custom
    /// module names, …). Best-effort: looks through every documented placement
    /// in the Mender JSON and falls back to `unknown`.
    pub fn kind(&self) -> String {
        self.extra
            .get("type_info")
            .and_then(|info| info.get("type"))
            .or_else(|| {
                self.extra
                    .get("info")
                    .and_then(|info| info.get("type_info"))
                    .and_then(|info| info.get("type"))
            })
            .or_else(|| {
                self.extra
                    .get("updates")
                    .and_then(Value::as_array)
                    .and_then(|updates| updates.first())
                    .and_then(|update| update.get("type_info"))
                    .and_then(|info| info.get("type"))
            })
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_owned()
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SortKey {
    Name,
    Type,
}

pub fn sort_artifacts(artifacts: &mut [Artifact], sort: SortKey) {
    match sort {
        SortKey::Name => artifacts.sort_by_key(|a| a.name.to_lowercase()),
        SortKey::Type => artifacts.sort_by(|a, b| {
            a.kind()
                .to_lowercase()
                .cmp(&b.kind().to_lowercase())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_the_update_type_from_every_documented_placement() {
        let top_level: Artifact = serde_json::from_value(json!({
            "id": "a", "type_info": {"type": "docker-compose"}
        }))
        .unwrap();
        let in_info: Artifact = serde_json::from_value(json!({
            "id": "b", "info": {"type_info": {"type": "script"}}
        }))
        .unwrap();
        let in_updates: Artifact = serde_json::from_value(json!({
            "id": "c", "updates": [{"type_info": {"type": "rootfs-image"}}]
        }))
        .unwrap();
        let missing: Artifact = serde_json::from_value(json!({"id": "d"})).unwrap();
        assert_eq!(top_level.kind(), "docker-compose");
        assert_eq!(in_info.kind(), "script");
        assert_eq!(in_updates.kind(), "rootfs-image");
        assert_eq!(missing.kind(), "unknown");
    }
}
