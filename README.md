# menderctl

`menderctl` is a native Rust command-line application for managing Mender
devices. It combines the official CLI workflows with a searchable interactive
interface inspired by `mender-richer-cli`, without invoking another CLI or
requiring a Python runtime.

## Features

- Full-screen, scrollable table picker with type-to-filter for devices and artifacts
- Favorite device shortcut for one-keystroke remote access
- Authentication and local token storage
- Device listing and searchable, scrollable inventory inspection
- Copy inventory values directly to the terminal clipboard
- Artifact listing, upload, download, deletion, and deployment
- Artifact sorting by name or update type (`--sort type`)
- Remote file upload and download
- Native remote terminal support
- TCP and UDP port forwarding through Mender protocols v1 and v2
- JSON output for automation
- Bash, Zsh, Fish, PowerShell, and Elvish completions

## Requirements

- Rust 1.90 or newer
- A Mender server account or API token
- Remote terminal, file transfer, and port forwarding enabled on target devices
  when using those features

## Installation

Install directly from the repository:

```bash
git clone git@github.com:YohanCamim/menderctl.git
cd menderctl
cargo install --path . --locked
```

Cargo installs the executable as `~/.cargo/bin/menderctl`. Make sure that
directory is in your `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

To make this permanent, add the same line to `~/.bashrc`, `~/.zshrc`, or the
configuration file for your shell.

Alternatively, build and install into `/usr/local/bin`:

```bash
make build
sudo make install
```

Verify the installation:

```bash
menderctl --version
menderctl --help
```

## Authentication

Log in with your Mender account:

```bash
menderctl --server https://hosted.mender.io login --username you@example.com
```

The token is stored in `$XDG_CACHE_HOME/mender/authtoken`, or in
`$HOME/.cache/mender/authtoken` when `XDG_CACHE_HOME` is not set.

For automation, provide an API token without writing a file:

```bash
MENDER_SERVER=https://hosted.mender.io \
MENDER_TOKEN="$MENDER_API_TOKEN" \
menderctl devices list --raw
```

If Mender returns `401 Unauthorized`, verify that the token belongs to the
selected server, then authenticate again:

```bash
menderctl --server https://your-mender-server.example.com login
```

## Configuration

`menderctl` reads the first configuration file found in this order:

1. `.menderctlrc` in the current directory
2. `.mender-clirc` in the current directory, for migration compatibility
3. `$HOME/.menderctlrc`
4. `$HOME/.mender-clirc`, for migration compatibility
5. `/etc/menderctl/.menderctlrc`
6. `/etc/mender/.mender-clirc`, for migration compatibility

Configuration files use JSON:

```json
{
  "server": "https://hosted.mender.io",
  "username": "you@example.com"
}
```

Effective precedence is: command-line arguments, `MENDER_SERVER` and
`MENDER_TOKEN`, configuration file, then defaults.

The favorite device for each Mender server is stored separately in
`$XDG_CONFIG_HOME/menderctl/favorites.json` (or
`$HOME/.config/menderctl/favorites.json`).

Use `--skip-verify` only with trusted development servers using a certificate
that cannot be verified normally.

## Usage

Start the interactive interface:

```bash
menderctl --server https://hosted.mender.io
```

In the device picker, press `Shift+F` on a device to mark or unmark it as the
favorite. The `★` marker identifies the current favorite. Connect to it
directly from any terminal with:

```bash
menderctl --favorite
```

The device and artifact tables use the available terminal height instead of
printing every result. Navigate with `↑`/`↓`, move one page with
`Page Up`/`Page Down`, and jump to either end with `Home`/`End`.

Device inventory opens in the same full-screen table browser. Select an
attribute and press `Shift+C` to copy its complete value to the terminal
clipboard. Lowercase `f` and `c` remain normal search characters.

### Interactive controls

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move by one row |
| `Page Up` / `Page Down` | Move by one visible page |
| `Home` / `End` | Jump to the first or last result |
| Type text | Filter the visible results |
| `Enter` | Select the highlighted device or artifact |
| `Shift+F` | Mark or unmark the highlighted device as favorite |
| `Shift+C` | Copy the highlighted inventory value |
| `Esc` | Close the current table, or return from the action menu to the device list |

Use commands directly in scripts:

```bash
# Devices
menderctl devices list
menderctl devices list --raw
menderctl devices inventory DEVICE_ID

# Artifacts and deployments
menderctl artifacts list
menderctl artifacts list --sort type
menderctl artifacts upload update.mender --description "Release 2.4"
menderctl artifacts download ARTIFACT_ID --destination-path .
menderctl artifacts deploy release-2.4 DEVICE_ID --yes
menderctl artifacts delete ARTIFACT_ID --yes

# Remote access
menderctl --favorite
menderctl terminal DEVICE_ID
menderctl port-forward DEVICE_ID 8080:80
menderctl port-forward DEVICE_ID 2222:192.168.1.10:22
menderctl port-forward DEVICE_ID udp/5353:53

# File transfer
menderctl cp ./config.toml DEVICE_ID:/etc/my-app/config.toml --yes
menderctl cp DEVICE_ID:/var/log/my-app.log . --yes
```

Global options can be placed before or after a subcommand.

## Shell completion

Generate a completion script for your shell:

```bash
menderctl completion bash > ~/.local/share/bash-completion/completions/menderctl
menderctl completion zsh > ~/.local/share/zsh/site-functions/_menderctl
```

The Makefile can install Bash and Zsh completions system-wide:

```bash
sudo make install-autocomplete-scripts
```

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
```

Or use the Makefile shortcuts:

```bash
make check
make build
```

The release binary is written to `target/release/menderctl` and copied to
`./menderctl` by `make build`.

## Docker

Build a minimal runtime image:

```bash
docker build -t menderctl .
docker run --rm menderctl --help
```

## License

Apache License 2.0. See [LICENSE](LICENSE).
