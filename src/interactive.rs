use std::path::PathBuf;

use anyhow::Result;
use dialoguer::{Confirm, FuzzySelect, Input, Select, theme::ColorfulTheme};

use crate::{
    api::MenderApi,
    favorite::{FavoriteDevice, Favorites},
    models::{InventoryDevice, SortKey, sort_artifacts},
    picker, portforward, terminal, ui,
};

pub async fn run(api: &MenderApi, server: &str, token: &str, skip_verify: bool) -> Result<()> {
    ui::welcome();
    let theme = ColorfulTheme::default();
    let mut favorites = Favorites::load()?;
    let mut initial_selection = 0;
    loop {
        let devices = api.search_devices().await?;
        if devices.is_empty() {
            ui::warning("No accepted devices were found.");
            return Ok(());
        }
        let choice = loop {
            let favorite_id = favorites
                .for_server(server)
                .map(|favorite| favorite.id.as_str());
            let favorite_index =
                favorite_id.and_then(|id| devices.iter().position(|device| device.id == id));
            let rows = devices
                .iter()
                .map(|device| {
                    vec![
                        device.display_name(),
                        device.id.clone(),
                        device.updated_at().unwrap_or("unknown").to_owned(),
                    ]
                })
                .collect::<Vec<_>>();
            let pick = tokio::task::spawn_blocking(move || {
                picker::pick_device(
                    "Select a device",
                    &["Name", "Device ID", "Last contact"],
                    rows,
                    favorite_index,
                    initial_selection,
                )
            })
            .await??;
            match pick {
                picker::DevicePick::Selected(index) => break Some(index),
                picker::DevicePick::Cancelled => break None,
                picker::DevicePick::ToggleFavorite(index) => {
                    initial_selection = index;
                    let device = &devices[index];
                    let name = device.display_name();
                    let is_favorite = favorites
                        .toggle(
                            server,
                            FavoriteDevice {
                                id: device.id.clone(),
                                name: name.clone(),
                            },
                        )?
                        .is_some();
                    if is_favorite {
                        ui::success(format!(
                            "{name} is now the favorite device; use `menderctl --favorite` to connect."
                        ));
                    } else {
                        ui::warning(format!("{name} is no longer the favorite device."));
                    }
                }
            }
        };
        let Some(choice) = choice else {
            break;
        };
        initial_selection = choice;
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
        ui::device_banner(&device.display_name(), &device.id, "Device session");
        let Some(action) = FuzzySelect::with_theme(theme)
            .with_prompt("Select an action")
            .items(actions)
            .default(0)
            .interact_opt()?
        else {
            return Ok(true);
        };
        let outcome = match action {
            0 => terminal::run(api, server, token, skip_verify, &device.id).await,
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
                .await
            }
            2 => upload_file(api, device, theme).await,
            3 => download_file(api, device, theme).await,
            4 => deploy_artifact(api, device, theme).await,
            5 => api.inventory(&device.id).await.and_then(|inventory| {
                ui::inventory_table(
                    &inventory,
                    &format!("Inventory for {} ({})", device.display_name(), device.id),
                )
            }),
            6 => return Ok(true),
            _ => return Ok(false),
        };
        if let Err(error) = outcome {
            ui::error(format!("{error:#}"));
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
    let ordering = Select::with_theme(theme)
        .with_prompt(format!("Order artifacts for {}", device.display_name()))
        .items(["Name", "Type"])
        .default(0)
        .interact()?;
    let mut artifacts = artifacts;
    sort_artifacts(
        &mut artifacts,
        if ordering == 0 {
            SortKey::Name
        } else {
            SortKey::Type
        },
    );
    let rows = artifacts
        .iter()
        .map(|artifact| {
            let kind = artifact.kind();
            let kind = if kind == "unknown" {
                "—".to_owned()
            } else {
                kind
            };
            vec![artifact.name.clone(), kind, artifact.description.clone()]
        })
        .collect::<Vec<_>>();
    let Some(choice) = tokio::task::spawn_blocking(move || {
        picker::pick("Select an artifact", &["Name", "Type", "Description"], rows)
    })
    .await??
    else {
        return Ok(());
    };
    let artifact = &artifacts[choice];
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
