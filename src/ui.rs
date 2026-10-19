use std::io::{self, IsTerminal};

use anyhow::Result;
use comfy_table::{
    Attribute, Cell, Color, ContentArrangement, Table, modifiers::UTF8_ROUND_CORNERS,
    presets::UTF8_FULL_CONDENSED,
};
use console::{Emoji, Style};
use serde_json::Value;

use crate::{
    models::{Artifact, Device, InventoryDevice},
    picker,
};

static OK: Emoji<'_, '_> = Emoji("✓ ", "OK ");
static WARNING: Emoji<'_, '_> = Emoji("! ", "! ");
static ERROR: Emoji<'_, '_> = Emoji("✗ ", "X ");

pub fn welcome() {
    let cyan = Style::new().cyan().bold();
    let dim = Style::new().dim();
    println!("{}", cyan.apply_to("╭──────────────────────────────╮"));
    println!("{}", cyan.apply_to("│    menderctl — native Rust   │"));
    println!("{}", cyan.apply_to("╰──────────────────────────────╯"));
    println!(
        "{}",
        dim.apply_to(format!(
            "v{} — device operations, transfers, and maintenance, natively.",
            env!("CARGO_PKG_VERSION")
        ))
    );
}

pub fn success(message: impl AsRef<str>) {
    println!(
        "{}{}",
        Style::new().green().apply_to(OK),
        Style::new().green().apply_to(message.as_ref())
    );
}

pub fn warning(message: impl AsRef<str>) {
    eprintln!(
        "{}{}",
        Style::new().yellow().apply_to(WARNING),
        Style::new().yellow().apply_to(message.as_ref())
    );
}

pub fn error(message: impl AsRef<str>) {
    eprintln!(
        "{}{}",
        Style::new().red().apply_to(ERROR),
        Style::new().red().apply_to(message.as_ref())
    );
}

/// Persistent device context box shown above any operation that targets a
/// device (terminal, port-forward, deploy, transfers).
pub fn device_banner(device_name: &str, device_id: &str, action: &str) {
    let mut table = styled_table();
    table.add_row(vec![
        Cell::new(format!("{device_name} ({device_id})"))
            .fg(Color::Cyan)
            .add_attribute(Attribute::Bold),
    ]);
    table.add_row(vec![Cell::new(action).fg(Color::DarkGrey)]);
    println!("\n{table}");
}

pub fn devices_table(devices: &[InventoryDevice]) {
    let mut table = styled_table();
    table.set_header(header(&["#", "Name", "Device ID", "Last contact"]));
    for (index, device) in devices.iter().enumerate() {
        table.add_row(vec![
            Cell::new(index + 1).fg(Color::Magenta),
            Cell::new(device.display_name()).fg(Color::Cyan),
            Cell::new(&device.id),
            Cell::new(device.updated_at().unwrap_or("unknown")).fg(Color::DarkGrey),
        ]);
    }
    println!("\n{table}");
}

pub fn legacy_devices_table(devices: &[Device]) {
    let mut table = styled_table();
    table.set_header(header(&["Device ID", "Status", "Identity", "Last update"]));
    for device in devices {
        let status_color = match device.status.as_str() {
            "accepted" => Some(Color::Green),
            "pending" | "preauthorized" => Some(Color::Yellow),
            "rejected" | "decommissioned" => Some(Color::Red),
            _ => None,
        };
        let status_cell = match status_color {
            Some(color) => Cell::new(&device.status).fg(color),
            None => Cell::new(&device.status),
        };
        table.add_row(vec![
            Cell::new(&device.id).fg(Color::Cyan),
            status_cell,
            Cell::new(compact_json(&device.identity_data)),
            Cell::new(&device.updated_ts),
        ]);
    }
    println!("{table}");
}

pub fn artifacts_table(artifacts: &[Artifact]) {
    let mut table = styled_table();
    table.set_header(header(&[
        "#",
        "Name",
        "Type",
        "Artifact ID",
        "Size",
        "Description",
    ]));
    for (index, artifact) in artifacts.iter().enumerate() {
        table.add_row(vec![
            Cell::new(index + 1).fg(Color::Magenta),
            Cell::new(&artifact.name).fg(Color::Cyan),
            Cell::new(artifact.kind()),
            Cell::new(&artifact.id),
            Cell::new(human_size(artifact.size)),
            Cell::new(if artifact.description.is_empty() {
                "—"
            } else {
                &artifact.description
            }),
        ]);
    }
    println!("\n{table}");
}

pub fn inventory_table(inventory: &Value, title: &str) -> Result<()> {
    let mut rows = Vec::new();
    flatten_inventory("", inventory, &mut rows);
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return picker::view(
            title,
            &["Attribute", "Value"],
            rows.into_iter()
                .map(|(attribute, value)| vec![attribute, value])
                .collect(),
            1,
        );
    }

    let mut table = styled_table();
    table.set_header(header(&["Attribute", "Value"]));
    for (key, value) in rows {
        table.add_row(vec![Cell::new(key).fg(Color::Cyan), Cell::new(value)]);
    }
    println!(
        "\n{}\n{table}",
        Style::new().bold().magenta().apply_to(title)
    );
    Ok(())
}

fn header(labels: &[&str]) -> Vec<Cell> {
    labels
        .iter()
        .map(|label| {
            Cell::new(label)
                .fg(Color::Cyan)
                .add_attribute(Attribute::Bold)
        })
        .collect()
}

fn styled_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}

fn flatten_inventory(prefix: &str, value: &Value, rows: &mut Vec<(String, String)>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_inventory(&key, value, rows);
            }
        }
        Value::Array(values) => {
            if values
                .iter()
                .all(|value| !value.is_object() && !value.is_array())
            {
                rows.push((
                    prefix.to_owned(),
                    values
                        .iter()
                        .map(compact_json)
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            } else {
                for (index, value) in values.iter().enumerate() {
                    flatten_inventory(&format!("{prefix}[{index}]"), value, rows);
                }
            }
        }
        _ => rows.push((prefix.to_owned(), compact_json(value))),
    }
}

fn compact_json(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "—".to_owned(),
        other => other.to_string(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
