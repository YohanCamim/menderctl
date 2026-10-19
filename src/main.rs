use std::{
    io::{self, IsTerminal},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use dialoguer::{Confirm, Input, theme::ColorfulTheme};
use tracing_subscriber::EnvFilter;

use menderctl::{
    api::MenderApi,
    cli::{ArtifactsCommand, Cli, Command, CompletionShell, DevicesCommand, LoginArgs},
    config::{RuntimeConfig, save_token},
    favorite::Favorites,
    interactive,
    models::sort_artifacts,
    portforward, terminal, ui,
};

#[tokio::main]
async fn main() {
    if let Err(error) = try_main().await {
        ui::error(format!("{error:#}"));
        std::process::exit(1);
    }
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    if cli.favorite && cli.command.is_some() {
        bail!("--favorite cannot be combined with a subcommand");
    }
    if !cli.favorite && cli.command.is_none() && !io::stdin().is_terminal() {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    }
    let config = RuntimeConfig::resolve(&cli)?;
    if cli.favorite {
        let favorite = Favorites::load()?
            .for_server(&config.server)
            .cloned()
            .with_context(|| {
                format!(
                    "no favorite device is configured for {}; open the interactive interface and press Shift+F on a device",
                    config.server
                )
            })?;
        let token = config.require_token()?.to_owned();
        let api = MenderApi::new(&config.server, &token, config.skip_verify)?;
        return terminal::run(
            &api,
            &config.server,
            &token,
            config.skip_verify,
            &favorite.id,
        )
        .await;
    }
    match cli.command.clone().unwrap_or(Command::Interactive) {
        Command::Completion { shell } => completion(shell),
        Command::Login(args) => login(&config, args).await,
        command => {
            let token = config.require_token()?.to_owned();
            let api = MenderApi::new(&config.server, &token, config.skip_verify)?;
            dispatch_authenticated(command, &config, &api, &token).await
        }
    }
}

async fn dispatch_authenticated(
    command: Command,
    config: &RuntimeConfig,
    api: &MenderApi,
    token: &str,
) -> Result<()> {
    match command {
        Command::Interactive => {
            interactive::run(api, &config.server, token, config.skip_verify).await
        }
        Command::Devices { command } => match command {
            DevicesCommand::List(args) => {
                if args.raw {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &api.list_devices_raw(args.page, args.per_page).await?
                        )?
                    );
                } else {
                    ui::legacy_devices_table(&api.list_devices(args.page, args.per_page).await?);
                }
                Ok(())
            }
            DevicesCommand::Inventory { device_id, raw } => {
                let inventory = api.inventory(&device_id).await?;
                if raw {
                    println!("{}", serde_json::to_string_pretty(&inventory)?);
                } else {
                    ui::inventory_table(&inventory, &format!("Inventory for {device_id}"))?;
                }
                Ok(())
            }
        },
        Command::Artifacts { command } => artifacts(api, command).await,
        Command::Terminal { device_id } => {
            terminal::run(api, &config.server, token, config.skip_verify, &device_id).await
        }
        Command::PortForward(args) => {
            portforward::run(
                api,
                &config.server,
                token,
                config.skip_verify,
                &args.device_id,
                &args.bind_host,
                &args.mappings,
            )
            .await
        }
        Command::Cp {
            source,
            destination,
            yes,
        } => copy(api, &source, &destination, yes).await,
        Command::Login(_) | Command::Completion { .. } => unreachable!(),
    }
}

async fn login(config: &RuntimeConfig, args: LoginArgs) -> Result<()> {
    let theme = ColorfulTheme::default();
    let username = match args.username.or_else(|| config.login_username.clone()) {
        Some(username) => username,
        None => Input::with_theme(&theme)
            .with_prompt("Email address")
            .interact_text()?,
    };
    let password = match args.password.or_else(|| config.login_password.clone()) {
        Some(password) => password,
        None => rpassword::prompt_password("Password: ")?,
    };
    let token = MenderApi::login(
        &config.server,
        &username,
        &password,
        args.two_factor_code.as_deref(),
        config.skip_verify,
    )
    .await?;
    save_token(&config.token_path, &token)?;
    ui::success(format!(
        "Login successful; token saved to {}.",
        config.token_path.display()
    ));
    Ok(())
}

async fn artifacts(api: &MenderApi, command: ArtifactsCommand) -> Result<()> {
    match command {
        ArtifactsCommand::List(args) => {
            if args.list.raw {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &api.list_artifacts_raw(args.list.page, args.list.per_page)
                            .await?
                    )?
                );
            } else {
                let mut artifacts = api
                    .list_artifacts(args.list.page, args.list.per_page)
                    .await?;
                sort_artifacts(&mut artifacts, args.sort);
                ui::artifacts_table(&artifacts);
            }
        }
        ArtifactsCommand::Upload {
            artifact,
            description,
            no_progress,
        } => {
            api.upload_artifact(&artifact, &description, !no_progress)
                .await?;
            ui::success("Artifact uploaded.");
        }
        ArtifactsCommand::Download {
            artifact_id,
            destination_path,
            no_progress,
        } => {
            let path = api
                .download_artifact(&artifact_id, &destination_path, !no_progress)
                .await?;
            ui::success(format!("Artifact saved to {}.", path.display()));
        }
        ArtifactsCommand::Delete { artifact_id, yes } => {
            if yes || confirm(&format!("Permanently delete artifact {artifact_id}?"))? {
                api.delete_artifact(&artifact_id).await?;
                ui::success("Artifact deleted.");
            }
        }
        ArtifactsCommand::Deploy {
            artifact_name,
            device_id,
            name,
            yes,
        } => {
            let device_name = api.display_name(&device_id).await;
            if yes
                || confirm(&format!(
                    "Deploy {artifact_name} to {device_name} ({device_id})?"
                ))?
            {
                api.deploy(&device_id, &artifact_name, name.as_deref())
                    .await?;
                ui::success("Deployment created.");
            }
        }
    }
    Ok(())
}

async fn copy(api: &MenderApi, source: &str, destination: &str, yes: bool) -> Result<()> {
    match (parse_remote(source), parse_remote(destination)) {
        (None, Some((device_id, remote))) => {
            let device_name = api.display_name(device_id).await;
            if yes
                || confirm(&format!(
                    "Upload {source} to {device_name} ({device_id}):{remote}?"
                ))?
            {
                api.upload_file(device_id, PathBuf::from(source).as_path(), remote)
                    .await?;
                ui::success("File uploaded.");
            }
        }
        (Some((device_id, remote)), None) => {
            let device_name = api.display_name(device_id).await;
            if yes
                || confirm(&format!(
                    "Download {device_name} ({device_id}):{remote} to {destination}?"
                ))?
            {
                let path = api
                    .download_file(device_id, remote, PathBuf::from(destination).as_path())
                    .await?;
                ui::success(format!("File saved to {}.", path.display()));
            }
        }
        (Some(_), Some(_)) => bail!("copying between two devices is not supported"),
        (None, None) => bail!("the source or destination must use DEVICE_ID:PATH"),
    }
    Ok(())
}

fn parse_remote(value: &str) -> Option<(&str, &str)> {
    let (device, path) = value.split_once(':')?;
    if device.is_empty() || path.is_empty() {
        None
    } else {
        Some((device, path))
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("cannot request confirmation without a terminal; add --yes");
    }
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()?)
}

fn completion(shell: CompletionShell) -> Result<()> {
    let shell = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
        CompletionShell::PowerShell => Shell::PowerShell,
        CompletionShell::Elvish => Shell::Elvish,
    };
    generate(shell, &mut Cli::command(), "menderctl", &mut io::stdout());
    Ok(())
}

fn init_logging(verbose: bool) {
    let filter = if verbose { "menderctl=debug" } else { "warn" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .with_target(false)
        .try_init();
}
