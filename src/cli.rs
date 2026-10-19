use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::models::SortKey;

#[derive(Clone, Debug, Parser)]
#[command(
    name = "menderctl",
    about = "A native, scriptable, and user-friendly CLI for Mender",
    version,
    propagate_version = true
)]
pub struct Cli {
    /// Root URL of the Mender server.
    #[arg(long, global = true, env = "MENDER_SERVER")]
    pub server: Option<String>,

    /// Disable TLS certificate verification.
    #[arg(short = 'k', long, global = true)]
    pub skip_verify: bool,

    /// File containing the JWT token.
    #[arg(long, global = true)]
    pub token: Option<PathBuf>,

    /// JWT token or API key.
    #[arg(long, global = true, env = "MENDER_TOKEN", hide_env_values = true)]
    pub token_value: Option<String>,

    /// Enable diagnostic logging.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Open a remote terminal on the favorite device.
    #[arg(long)]
    pub favorite: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Open the interactive searchable interface.
    Interactive,
    /// Authenticate and save the token locally.
    Login(LoginArgs),
    /// Browse devices and their inventory.
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Manage artifacts and deployments.
    Artifacts {
        #[command(subcommand)]
        command: ArtifactsCommand,
    },
    /// Inspect, follow, and cancel deployments.
    Deployments {
        #[command(subcommand)]
        command: DeploymentsCommand,
    },
    /// Open a native remote terminal.
    Terminal { device_id: String },
    /// Forward local TCP or UDP ports to a device.
    PortForward(PortForwardArgs),
    /// Copy a file to or from a device.
    Cp {
        source: String,
        destination: String,
        /// Run without asking for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Generate shell completions.
    Completion { shell: CompletionShell },
}

#[derive(Clone, Debug, Args)]
pub struct LoginArgs {
    #[arg(long)]
    pub username: Option<String>,
    #[arg(long, hide = true)]
    pub password: Option<String>,
    #[arg(long = "2fa-code")]
    pub two_factor_code: Option<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum DevicesCommand {
    /// List devices.
    List(ListArgs),
    /// Show the complete inventory for a device.
    Inventory {
        device_id: String,
        #[arg(long)]
        raw: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub struct ListArgs {
    #[arg(short = 'N', long, default_value_t = 20)]
    pub per_page: usize,
    #[arg(short = 'P', long, default_value_t = 1)]
    pub page: usize,
    #[arg(short, long)]
    pub raw: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ArtifactListArgs {
    #[command(flatten)]
    pub list: ListArgs,
    /// Sort the artifact list.
    #[arg(long, value_enum, default_value_t = SortKey::Name)]
    pub sort: SortKey,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ArtifactsCommand {
    /// List available artifacts.
    List(ArtifactListArgs),
    /// Upload an artifact to the server.
    Upload {
        artifact: PathBuf,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long)]
        no_progress: bool,
    },
    /// Download an artifact by ID.
    Download {
        artifact_id: String,
        #[arg(long, default_value = ".")]
        destination_path: PathBuf,
        #[arg(long)]
        no_progress: bool,
    },
    /// Delete an artifact by ID.
    Delete {
        artifact_id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Create a deployment for one or more devices.
    Deploy {
        artifact_name: String,
        #[arg(required = true)]
        device_ids: Vec<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub struct DeploymentListArgs {
    #[command(flatten)]
    pub list: ListArgs,
    /// Only show deployments with this aggregate status.
    #[arg(long, value_enum)]
    pub status: Option<DeploymentStatus>,
    /// Search deployment names and descriptions.
    #[arg(long)]
    pub search: Option<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum DeploymentsCommand {
    /// List deployments.
    List(DeploymentListArgs),
    /// Show deployment details and device statistics.
    Show {
        deployment_id: String,
        #[arg(long)]
        raw: bool,
    },
    /// Refresh deployment progress until it finishes or Ctrl+C is pressed.
    Watch {
        deployment_id: String,
        /// Refresh interval in seconds.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
    },
    /// Cancel a deployment that has not finished yet.
    Cancel {
        deployment_id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DeploymentStatus {
    Scheduled,
    Pending,
    #[value(name = "inprogress")]
    InProgress,
    Finished,
}

impl DeploymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Pending => "pending",
            Self::InProgress => "inprogress",
            Self::Finished => "finished",
        }
    }
}

#[derive(Clone, Debug, Args)]
pub struct PortForwardArgs {
    pub device_id: String,
    /// Mappings in the form `[tcp/]LOCAL_PORT[:REMOTE_HOST:REMOTE_PORT]`.
    #[arg(required = true)]
    pub mappings: Vec<String>,
    #[arg(long = "bind", default_value = "127.0.0.1")]
    pub bind_host: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_creation_accepts_multiple_device_ids() {
        let cli = Cli::try_parse_from([
            "menderctl",
            "artifacts",
            "deploy",
            "release-2.4",
            "device-1",
            "device-2",
            "--yes",
        ])
        .unwrap();
        let Some(Command::Artifacts {
            command:
                ArtifactsCommand::Deploy {
                    artifact_name,
                    device_ids,
                    yes,
                    ..
                },
        }) = cli.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(artifact_name, "release-2.4");
        assert_eq!(device_ids, ["device-1", "device-2"]);
        assert!(yes);
    }

    #[test]
    fn deployment_list_accepts_the_api_inprogress_spelling() {
        let cli =
            Cli::try_parse_from(["menderctl", "deployments", "list", "--status", "inprogress"])
                .unwrap();
        let Some(Command::Deployments {
            command: DeploymentsCommand::List(args),
        }) = cli.command
        else {
            panic!("unexpected command");
        };
        assert_eq!(
            args.status.map(DeploymentStatus::as_str),
            Some("inprogress")
        );
    }
}
