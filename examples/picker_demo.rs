//! Renders the picker with fake data in a real terminal for manual or PTY
//! testing. Run with `cargo run --example picker_demo`.
use menderctl::picker;

fn main() -> anyhow::Result<()> {
    let rows = vec![
        vec![
            "raspberry-pi-01".to_owned(),
            "61b3f041a2b3c4d5e6f7089a2b3c4d5e".to_owned(),
            "2026-10-19 16:42".to_owned(),
        ],
        vec![
            "gateway-warehouse".to_owned(),
            "9f2ce7d00123456789abcdef01234567".to_owned(),
            "2026-10-19 15:01".to_owned(),
        ],
        vec![
            "beacon-hallway-3".to_owned(),
            "00000000deadbeefcafebabef00dbabe".to_owned(),
            "2026-10-18 09:30".to_owned(),
        ],
        vec![
            "jetson-edge-04".to_owned(),
            "cafebabef00dfeed1234567890abcdef".to_owned(),
            "2026-10-19 08:12".to_owned(),
        ],
    ];
    match picker::pick(
        "Select a device",
        &["Name", "Device ID", "Last contact"],
        rows,
    )? {
        Some(index) => println!("\rpicked index {index}"),
        None => println!("\rcancelled"),
    }
    Ok(())
}
