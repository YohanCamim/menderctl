use std::path::PathBuf;

use anyhow::{Context, Result};
use dialoguer::{Confirm, FuzzySelect, Input, theme::ColorfulTheme};

use crate::{api::MenderApi, models::InventoryDevice, portforward, terminal, ui};

pub async fn run(api: &MenderApi, server: &str, token: &str, skip_verify: bool) -> Result<()> {
    ui::welcome();
    let theme = ColorfulTheme::default();
    loop {
        let devices = api.search_devices().await?;
        if devices.is_empty() {
            ui::warning("No accepted devices were found.");
            return Ok(());
        }
        ui::devices_table(&devices);
        let labels = devices
            .iter()
            .map(|device| format!("{}  ·  {}", device.display_name(), device.id))
            .chain(std::iter::once("Quit".to_owned()))
            .collect::<Vec<_>>();
        let choice = FuzzySelect::with_theme(&theme)
            .with_prompt("Search for and select a device")
            .items(&labels)
            .default(0)
            .interact()?;
        if choice == devices.len() {
            break;
        }
        let device = &devices[choice];
        if !device_menu(api, server, token, skip_verify, device, &theme).await? {
            break;
        }
    }
    ui::success("Goodbye.");
    Ok(())
}

async fn device_menu(
    api: &MenderApi,
    server: &str,
    token: &str,
    skip_verify: bool,
    device: &InventoryDevice,
    theme: &ColorfulTheme,
) -> Result<bool> {
    let actions = [
        "Remote terminal",
        "TCP port forwarding",
        "Upload a file",
        "Download a file",
        "Deploy an artifact",
        "Show inventory",
        "Choose another device",
        "Quit",
    ];
    loop {
        let action = FuzzySelect::with_theme(theme)
            .with_prompt(format!("Action for {}", device.display_name()))
            .items(actions)
            .default(0)
            .interact()?;
        match action {
            0 => terminal::run(api, server, token, skip_verify, &device.id).await?,
            1 => {
                let mapping: String = Input::with_theme(theme)
                    .with_prompt("Local port[:remote host:remote port]")
                    .default("8000:8000".to_owned())
                    .interact_text()?;
                portforward::run(
                    api,
                    server,
                    token,
                    skip_verify,
                    &device.id,
                    "127.0.0.1",
                    &[mapping],
                )
                .await?;
            }
            2 => upload_file(api, device, theme).await?,
            3 => download_file(api, device, theme).await?,
            4 => deploy_artifact(api, device, theme).await?,
            5 => {
                let inventory = api.inventory(&device.id).await?;
                ui::inventory_table(
                    &inventory,
                    &format!("Inventory for {} ({})", device.display_name(), device.id),
                );
            }
            6 => return Ok(true),
            _ => return Ok(false),
        }
    }
}

async fn upload_file(
    api: &MenderApi,
    device: &InventoryDevice,
    theme: &ColorfulTheme,
) -> Result<()> {
    let local: String = Input::with_theme(theme)
        .with_prompt("Local file")
        .validate_with(|value: &String| -> std::result::Result<(), &str> {
            if PathBuf::from(value).is_file() {
                Ok(())
            } else {
                Err("this file does not exist")
            }
        })
        .interact_text()?;
    let remote: String = Input::with_theme(theme)
        .with_prompt("Remote path")
        .interact_text()?;
    if Confirm::with_theme(theme)
        .with_prompt(format!(
            "Upload {local} to {}:{remote}?",
            device.display_name()
        ))
        .default(true)
        .interact()?
    {
        api.upload_file(&device.id, PathBuf::from(local).as_path(), &remote)
            .await?;
        ui::success("File uploaded.");
    }
    Ok(())
}

async fn download_file(
    api: &MenderApi,
    device: &InventoryDevice,
    theme: &ColorfulTheme,
) -> Result<()> {
    let remote: String = Input::with_theme(theme)
        .with_prompt("Remote path")
        .interact_text()?;
    let local: String = Input::with_theme(theme)
        .with_prompt("Local destination")
        .default(".".to_owned())
        .interact_text()?;
    if Confirm::with_theme(theme)
        .with_prompt(format!(
            "Download {}:{remote} to {local}?",
            device.display_name()
        ))
        .default(true)
        .interact()?
    {
        let path = api
            .download_file(&device.id, &remote, PathBuf::from(local).as_path())
            .await?;
        ui::success(format!("File saved to {}.", path.display()));
    }
    Ok(())
}

async fn deploy_artifact(
    api: &MenderApi,
    device: &InventoryDevice,
    theme: &ColorfulTheme,
) -> Result<()> {
    let artifacts = api.list_artifacts(1, 500).await?;
    if artifacts.is_empty() {
        ui::warning("No artifacts are available.");
        return Ok(());
    }
    ui::artifacts_table(&artifacts);
    let labels = artifacts
        .iter()
        .map(|artifact| format!("{}  ·  {}", artifact.name, artifact.description))
        .collect::<Vec<_>>();
    let choice = FuzzySelect::with_theme(theme)
        .with_prompt("Artifact to deploy")
        .items(&labels)
        .default(0)
        .interact()?;
    let artifact = artifacts
        .get(choice)
        .context("invalid artifact selection")?;
    if Confirm::with_theme(theme)
        .with_prompt(format!(
            "Deploy {} to {}?",
            artifact.name,
            device.display_name()
        ))
        .default(false)
        .interact()?
    {
        api.deploy(&device.id, &artifact.name, None).await?;
        ui::success(format!("Deployment of {} created.", artifact.name));
    }
    Ok(())
}
