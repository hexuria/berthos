//! Map protocol actions onto the guest driver.
//!
//! Every argv starts at `/usr/local/bin/driver` inside the isolated Linux
//! guest. Host `xdotool`, host `import`, and host `DISPLAY` are never used.

/// Guest driver seam (`action.sh` behind this symlink).
pub const ACTION_BIN: &str = "/usr/local/bin/driver";
/// PNG signature used to reject non-frames.
pub const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
/// 1×1 transparent PNG returned by the in-memory guest.
pub const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Mouse button for a guest click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Middle / wheel.
    Middle,
}

/// An input or capture op that runs *inside* the live guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestOp {
    /// PNG of the guest Xvfb.
    Screenshot,
    /// Click at last-frame pixel coordinates (origin top-left).
    Click {
        /// Horizontal pixel.
        x: i32,
        /// Vertical pixel.
        y: i32,
        /// Which button.
        button: Button,
    },
    /// Type into the guest.
    Type {
        /// Text to type. Must be non-empty.
        text: String,
    },
    /// Key or chord in the guest.
    Key {
        /// Key names (`Return`, `ctrl`, `s`).
        keys: Vec<String>,
    },
}

/// Why an op cannot be turned into guest argv.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// Caller asked for something the driver will not do.
    #[error("{0}")]
    Invalid(String),
}

/// Map a guest op onto `driver` argv (including the binary).
pub fn action_argv(op: &GuestOp) -> Result<Vec<String>, ActionError> {
    match op {
        GuestOp::Screenshot => Ok(vec![ACTION_BIN.to_string(), "screenshot".into()]),
        GuestOp::Click { x, y, button } => Ok(vec![
            ACTION_BIN.to_string(),
            "click".into(),
            x.to_string(),
            y.to_string(),
            button_arg(*button).into(),
        ]),
        GuestOp::Type { text } => {
            if text.is_empty() {
                return Err(ActionError::Invalid("text is empty".into()));
            }
            Ok(vec![ACTION_BIN.to_string(), "type".into(), text.clone()])
        }
        GuestOp::Key { keys } => {
            if keys.is_empty() || keys.iter().all(|k| k.trim().is_empty()) {
                return Err(ActionError::Invalid("keys is empty".into()));
            }
            let mut argv = vec![ACTION_BIN.to_string(), "key".into()];
            argv.extend(keys.iter().cloned());
            Ok(argv)
        }
    }
}

fn button_arg(button: Button) -> &'static str {
    match button {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
    }
}

/// Parse a button name. Default left.
pub fn parse_button(raw: Option<&str>) -> Result<Button, ActionError> {
    match raw.unwrap_or("left") {
        "left" => Ok(Button::Left),
        "right" => Ok(Button::Right),
        "middle" => Ok(Button::Middle),
        other => Err(ActionError::Invalid(format!(
            "unknown button `{other}`; expected left, right, or middle"
        ))),
    }
}

/// True when `argv` would talk to a host display instead of the guest driver.
pub fn argv_targets_host(argv: &[String]) -> bool {
    if argv.is_empty() {
        return true;
    }
    if argv[0] != ACTION_BIN {
        return true;
    }
    argv.iter().any(|a| {
        let lower = a.to_ascii_lowercase();
        lower.contains(".x11-unix")
            || lower.contains("/tmp/.x11")
            || lower.contains("wayland")
            || lower.starts_with("display=")
    })
}

/// PNG width/height from IHDR, if this is a PNG.
pub fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || !data.starts_with(PNG_MAGIC) {
        return None;
    }
    if &data[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_screenshot_click_type_key() {
        assert_eq!(
            action_argv(&GuestOp::Screenshot).unwrap(),
            [ACTION_BIN, "screenshot"]
        );
        assert_eq!(
            action_argv(&GuestOp::Click {
                x: 10,
                y: 20,
                button: Button::Right,
            })
            .unwrap(),
            [ACTION_BIN, "click", "10", "20", "right"]
        );
        assert_eq!(
            action_argv(&GuestOp::Type {
                text: "hello".into(),
            })
            .unwrap(),
            [ACTION_BIN, "type", "hello"]
        );
        assert_eq!(
            action_argv(&GuestOp::Key {
                keys: vec!["META".into(), "s".into()],
            })
            .unwrap(),
            [ACTION_BIN, "key", "META", "s"]
        );
    }

    #[test]
    fn rejects_empty_type_and_keys() {
        assert!(action_argv(&GuestOp::Type {
            text: String::new()
        })
        .is_err());
        assert!(action_argv(&GuestOp::Key { keys: vec![] }).is_err());
    }

    #[test]
    fn host_display_argv_is_refused() {
        assert!(argv_targets_host(&[]));
        assert!(argv_targets_host(&[
            "xdotool".into(),
            "click".into(),
            "1".into()
        ]));
        assert!(argv_targets_host(&[
            ACTION_BIN.into(),
            "screenshot".into(),
            "DISPLAY=:0".into()
        ]));
        assert!(!argv_targets_host(&[
            ACTION_BIN.into(),
            "screenshot".into()
        ]));
    }

    #[test]
    fn png_ihdr() {
        assert_eq!(png_dimensions(MINIMAL_PNG), Some((1, 1)));
        assert_eq!(png_dimensions(b"not png"), None);
    }
}
