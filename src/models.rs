use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: Option<usize>,
}

impl<T> Page<T> {
    pub fn has_more(&self, page: usize, per_page: usize) -> bool {
        self.total
            .map(|total| page.saturating_mul(per_page) < total)
            .unwrap_or_else(|| self.items.len() == per_page)
    }
}

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Deployment {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artifact_name: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub finished: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub device_count: usize,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub statistics: Option<DeploymentStatistics>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Deployment {
    pub fn is_finished(&self) -> bool {
        self.finished.is_some() || self.status.eq_ignore_ascii_case("finished")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DeploymentStatistics {
    #[serde(default)]
    pub success: u64,
    #[serde(default)]
    pub pending: u64,
    #[serde(default)]
    pub failure: u64,
    #[serde(default)]
    pub downloading: u64,
    #[serde(default)]
    pub installing: u64,
    #[serde(default)]
    pub rebooting: u64,
    #[serde(default)]
    pub noartifact: u64,
    #[serde(rename = "already-installed", alias = "already_installed", default)]
    pub already_installed: u64,
    #[serde(default)]
    pub aborted: u64,
    #[serde(default)]
    pub pause_before_installing: u64,
    #[serde(default)]
    pub pause_before_rebooting: u64,
    #[serde(default)]
    pub pause_before_committing: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl DeploymentStatistics {
    pub fn completed(&self) -> u64 {
        self.success + self.failure + self.noartifact + self.already_installed + self.aborted
    }

    pub fn active(&self) -> u64 {
        self.downloading
            + self.installing
            + self.rebooting
            + self.pause_before_installing
            + self.pause_before_rebooting
            + self.pause_before_committing
    }
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

    #[test]
    fn paginated_results_use_total_when_the_server_provides_it() {
        let page = Page {
            items: vec![1, 2],
            total: Some(5),
        };
        assert!(page.has_more(1, 2));
        assert!(!page.has_more(3, 2));
    }

    #[test]
    fn deployment_statistics_count_terminal_and_active_states() {
        let statistics = DeploymentStatistics {
            success: 2,
            failure: 1,
            downloading: 3,
            already_installed: 4,
            ..DeploymentStatistics::default()
        };
        assert_eq!(statistics.completed(), 7);
        assert_eq!(statistics.active(), 3);
    }
}
