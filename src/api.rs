use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, Response, StatusCode, multipart};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::{fs, io::AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::models::{Artifact, DeploymentRequest, Device, DownloadLink, InventoryDevice};

const INVENTORY_SEARCH: &str = "/api/management/v2/inventory/filters/search";
const INVENTORY_DEVICE: &str = "/api/management/v1/inventory/devices";
const DEVICES: &str = "/api/management/v2/devauth/devices";
const DEVICE_CONNECT: &str = "/api/management/v1/deviceconnect/devices";
const ARTIFACTS: &str = "/api/management/v1/deployments/artifacts";
const ARTIFACT_LIST: &str = "/api/management/v1/deployments/artifacts/list";
const DEPLOYMENTS: &str = "/api/management/v1/deployments/deployments";
const LOGIN: &str = "/api/management/v1/useradm/auth/login";

#[derive(Clone)]
pub struct MenderApi {
    server: String,
    token: String,
    client: Client,
}

impl MenderApi {
    pub fn new(
        server: impl Into<String>,
        token: impl Into<String>,
        skip_verify: bool,
    ) -> Result<Self> {
        let client = Client::builder()
            .danger_accept_invalid_certs(skip_verify)
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("menderctl/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            server: server.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            client,
        })
    }

    pub async fn login(
        server: &str,
        username: &str,
        password: &str,
        two_factor_code: Option<&str>,
        skip_verify: bool,
    ) -> Result<Vec<u8>> {
        let client = Client::builder()
            .danger_accept_invalid_certs(skip_verify)
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut request = client
            .post(format!("{}{LOGIN}", server.trim_end_matches('/')))
            .basic_auth(username, Some(password))
            .header("Content-Type", "application/json");
        if let Some(code) = two_factor_code.filter(|code| !code.is_empty()) {
            request = request.json(&json!({ "token2fa": code }));
        }
        let response = request
            .send()
            .await
            .context("failed to connect to the server")?;
        let response = ensure_status(response, &[StatusCode::OK]).await?;
        Ok(response.bytes().await?.to_vec())
    }

    pub async fn list_devices(&self, page: usize, per_page: usize) -> Result<Vec<Device>> {
        validate_pagination(page, per_page)?;
        self.get_json(
            &format!("{DEVICES}?page={page}&per_page={per_page}"),
            &[StatusCode::OK],
        )
        .await
    }

    pub async fn list_devices_raw(&self, page: usize, per_page: usize) -> Result<Value> {
        validate_pagination(page, per_page)?;
        self.get_json(
            &format!("{DEVICES}?page={page}&per_page={per_page}"),
            &[StatusCode::OK],
        )
        .await
    }

    pub async fn search_devices(&self) -> Result<Vec<InventoryDevice>> {
        let body = json!({
            "page": 1,
            "per_page": 500,
            "filters": [{
                "scope": "identity",
                "attribute": "status",
                "type": "$eq",
                "value": "accepted"
            }],
            "attributes": [
                {"scope": "identity", "attribute": "status"},
                {"scope": "inventory", "attribute": "artifact_name"},
                {"scope": "inventory", "attribute": "hostname"},
                {"scope": "tags", "attribute": "name"},
                {"scope": "identity", "attribute": "id"},
                {"scope": "system", "attribute": "updated_ts"}
            ]
        });
        let response = self
            .authorized(self.client.post(self.url(INVENTORY_SEARCH)))
            .json(&body)
            .send()
            .await?;
        let response = ensure_status(response, &[StatusCode::OK]).await?;
        let mut devices: Vec<InventoryDevice> = response.json().await?;
        devices.sort_by_key(|device| device.display_name().to_lowercase());
        Ok(devices)
    }

    pub async fn inventory(&self, device_id: &str) -> Result<Value> {
        self.get_json(
            &format!("{INVENTORY_DEVICE}/{device_id}"),
            &[StatusCode::OK],
        )
        .await
    }

    pub async fn device_connected(&self, device_id: &str) -> Result<bool> {
        let value: Value = self
            .get_json(&format!("{DEVICE_CONNECT}/{device_id}"), &[StatusCode::OK])
            .await?;
        Ok(value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("connected")))
    }

    pub async fn list_artifacts(&self, page: usize, per_page: usize) -> Result<Vec<Artifact>> {
        validate_pagination(page, per_page)?;
        self.get_json(
            &format!("{ARTIFACT_LIST}?page={page}&per_page={per_page}"),
            &[StatusCode::OK],
        )
        .await
    }

    pub async fn list_artifacts_raw(&self, page: usize, per_page: usize) -> Result<Value> {
        validate_pagination(page, per_page)?;
        self.get_json(
            &format!("{ARTIFACT_LIST}?page={page}&per_page={per_page}"),
            &[StatusCode::OK],
        )
        .await
    }

    pub async fn upload_artifact(
        &self,
        path: &Path,
        description: &str,
        show_progress: bool,
    ) -> Result<()> {
        validate_artifact(path).await?;
        let metadata = fs::metadata(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        let file = fs::File::open(path).await?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid artifact filename")?
            .to_owned();

        let progress = progress_bar(metadata.len(), show_progress, "Uploading");
        let stream = ReaderStream::new(file).map({
            let progress = progress.clone();
            move |chunk| {
                if let Ok(ref bytes) = chunk {
                    progress.inc(bytes.len() as u64);
                }
                chunk
            }
        });
        let body = reqwest::Body::wrap_stream(stream);
        let part = multipart::Part::stream_with_length(body, metadata.len())
            .file_name(file_name)
            .mime_str("application/vnd.mender-artifact")?;
        let form = multipart::Form::new()
            .text("size", metadata.len().to_string())
            .text("description", description.to_owned())
            .part("artifact", part);
        let response = self
            .authorized(self.client.post(self.url(ARTIFACTS)))
            .multipart(form)
            .send()
            .await;
        progress.finish_and_clear();
        let response = response.context("artifact upload failed")?;
        ensure_status(response, &[StatusCode::CREATED]).await?;
        Ok(())
    }

    pub async fn delete_artifact(&self, artifact_id: &str) -> Result<()> {
        let response = self
            .authorized(
                self.client
                    .delete(self.url(&format!("{ARTIFACTS}/{artifact_id}"))),
            )
            .send()
            .await?;
        ensure_status(response, &[StatusCode::NO_CONTENT]).await?;
        Ok(())
    }

    pub async fn download_artifact(
        &self,
        artifact_id: &str,
        destination: &Path,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let link: DownloadLink = self
            .get_json(
                &format!("{ARTIFACTS}/{artifact_id}/download"),
                &[StatusCode::OK],
            )
            .await?;
        let artifact: Artifact = self
            .get_json(&format!("{ARTIFACTS}/{artifact_id}"), &[StatusCode::OK])
            .await?;
        let destination = if destination.is_dir() {
            destination.join(format!("{}.mender", artifact.name))
        } else {
            destination.to_owned()
        };
        let response =
            ensure_status(self.client.get(link.uri).send().await?, &[StatusCode::OK]).await?;
        write_download(response, &destination, artifact.size, show_progress).await?;
        Ok(destination)
    }

    pub async fn deploy(
        &self,
        device_id: &str,
        artifact_name: &str,
        deployment_name: Option<&str>,
    ) -> Result<()> {
        let request = DeploymentRequest {
            name: deployment_name
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("Deployment of {artifact_name} to {device_id}")),
            artifact_name,
            devices: vec![device_id],
        };
        let response = self
            .authorized(self.client.post(self.url(DEPLOYMENTS)))
            .json(&request)
            .send()
            .await?;
        ensure_status(response, &[StatusCode::CREATED]).await?;
        Ok(())
    }

    pub async fn upload_file(&self, device_id: &str, local: &Path, remote: &str) -> Result<()> {
        self.ensure_connected(device_id).await?;
        let metadata = fs::metadata(local)
            .await
            .with_context(|| format!("local file not found: {}", local.display()))?;
        let file_name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("invalid local filename")?
            .to_owned();
        let file = fs::File::open(local).await?;
        let stream = ReaderStream::new(file);
        let part =
            multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), metadata.len())
                .file_name(file_name);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            format!("{:o}", metadata.permissions().mode())
        };
        #[cfg(not(unix))]
        let mode = "644".to_owned();
        let form = multipart::Form::new()
            .text("path", remote.to_owned())
            .text("mode", mode)
            .part("file", part);
        let response = self
            .authorized(
                self.client
                    .put(self.url(&format!("{DEVICE_CONNECT}/{device_id}/upload"))),
            )
            .multipart(form)
            .send()
            .await?;
        ensure_status(response, &[StatusCode::CREATED]).await?;
        Ok(())
    }

    pub async fn download_file(
        &self,
        device_id: &str,
        remote: &str,
        local: &Path,
    ) -> Result<PathBuf> {
        self.ensure_connected(device_id).await?;
        let response = self
            .authorized(
                self.client
                    .get(self.url(&format!("{DEVICE_CONNECT}/{device_id}/download"))),
            )
            .query(&[("path", remote)])
            .send()
            .await?;
        let response = ensure_status(response, &[StatusCode::OK]).await?;
        let expected_size = response
            .headers()
            .get("X-MEN-FILE-SIZE")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .or_else(|| response.content_length())
            .unwrap_or(0);
        let local = if local.is_dir() {
            let name = Path::new(remote)
                .file_name()
                .context("remote path does not contain a filename")?;
            local.join(name)
        } else {
            local.to_owned()
        };
        write_download(response, &local, expected_size, true).await?;
        Ok(local)
    }

    async fn ensure_connected(&self, device_id: &str) -> Result<()> {
        if !self.device_connected(device_id).await? {
            bail!("device {device_id} is not connected");
        }
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        expected: &[StatusCode],
    ) -> Result<T> {
        let response = self
            .authorized(self.client.get(self.url(path)))
            .send()
            .await?;
        let response = ensure_status(response, expected).await?;
        response
            .json()
            .await
            .context("invalid JSON response from Mender")
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server, path)
    }
}

fn validate_pagination(page: usize, per_page: usize) -> Result<()> {
    if page == 0 || per_page == 0 {
        bail!("page and per-page must be greater than zero");
    }
    Ok(())
}

async fn ensure_status(response: Response, expected: &[StatusCode]) -> Result<Response> {
    if expected.contains(&response.status()) {
        return Ok(response);
    }
    let status = response.status();
    let url = response.url().clone();
    let request_id = response
        .headers()
        .get("X-MEN-RequestID")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();
    let details = if body.is_empty() {
        String::new()
    } else {
        format!(": {}", truncate(body, 500))
    };
    let request_id = request_id
        .map(|id| format!(" (request-id: {id})"))
        .unwrap_or_default();
    let hint = if status == StatusCode::UNAUTHORIZED {
        " Authentication failed. Verify the selected server and run `menderctl login` again."
    } else {
        ""
    };
    Err(anyhow!(
        "Mender returned HTTP {status} for {url}{request_id}{details}.{hint}"
    ))
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let text: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

fn progress_bar(length: u64, enabled: bool, message: &'static str) -> ProgressBar {
    if !enabled {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(length);
    progress.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} {eta}",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    progress.set_message(message);
    progress
}

async fn write_download(
    response: Response,
    destination: &Path,
    expected: u64,
    show_progress: bool,
) -> Result<()> {
    if let Some(parent) = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).await?;
    }
    let mut file = fs::File::create(destination)
        .await
        .with_context(|| format!("failed to create {}", destination.display()))?;
    let progress = progress_bar(expected, show_progress, "Downloading");
    let mut received = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;
        progress.set_position(received);
    }
    file.flush().await?;
    progress.finish_and_clear();
    if expected > 0 && received != expected {
        bail!("download size mismatch: received {received} bytes, expected {expected}");
    }
    Ok(())
}

async fn validate_artifact(path: &Path) -> Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let mut archive = tar::Archive::new(file);
        let Some(entry) = archive.entries()?.next() else {
            bail!("empty artifact");
        };
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new("version") {
            bail!("invalid artifact format: the `version` entry is missing");
        }
        let version: Value =
            serde_json::from_reader(&mut entry).context("invalid `version` entry")?;
        if version.get("format").and_then(Value::as_str) != Some("mender") {
            bail!("invalid artifact format");
        }
        Ok(())
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    use super::MenderApi;

    async fn mock_server(status: &str, body: &str) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = oneshot::channel();
        let status = status.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0_u8; 8192];
            let read = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let _ = request_tx.send(request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{address}"), request_rx)
    }

    #[tokio::test]
    async fn lists_devices_with_authentication_and_pagination() {
        let (server, request) = mock_server(
            "200 OK",
            r#"[{"id":"device-1","status":"accepted","identity_data":{"sn":"abc"}}]"#,
        )
        .await;
        let api = MenderApi::new(server, "secret-token", false).unwrap();
        let devices = api.list_devices(2, 50).await.unwrap();
        assert_eq!(devices[0].id, "device-1");
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /api/management/v2/devauth/devices?page=2&per_page=50"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer secret-token")
        );
    }

    #[tokio::test]
    async fn includes_server_error_details() {
        let (server, _) = mock_server("403 Forbidden", r#"{"error":"permission denied"}"#).await;
        let api = MenderApi::new(server, "token", false).unwrap();
        let error = api.list_devices(1, 20).await.unwrap_err().to_string();
        assert!(error.contains("403 Forbidden"));
        assert!(error.contains("permission denied"));
    }

    #[tokio::test]
    async fn explains_how_to_recover_from_an_unauthorized_token() {
        let (server, _) = mock_server("401 Unauthorized", r#"{"error":"unauthorized"}"#).await;
        let api = MenderApi::new(server, "expired-token", false).unwrap();
        let error = api.list_devices(1, 20).await.unwrap_err().to_string();
        assert!(error.contains("401 Unauthorized"));
        assert!(error.contains("menderctl login"));
    }
}
