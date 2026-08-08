use serde::{Deserialize, Serialize};

use crate::types::{TerminalError, MAX_INPUT_BYTES};

/// A modifier held while sending a typed key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    /// Control.
    #[serde(alias = "control")]
    Ctrl,
    /// Alt/Option.
    #[serde(alias = "option")]
    Alt,
    /// Shift.
    Shift,
    /// Super/Command/Windows/Meta.
    #[serde(alias = "command", alias = "cmd", alias = "meta", alias = "windows")]
    Super,
}

/// Named terminal key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedKey {
    /// Enter/Return.
    #[serde(alias = "return")]
    Enter,
    /// Escape.
    #[serde(alias = "esc")]
    Escape,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Insert.
    Insert,
    /// Delete.
    Delete,
}

/// One key identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyCode {
    /// A named terminal key.
    Named {
        /// Named key value.
        name: NamedKey,
    },
    /// One Unicode scalar value.
    Character {
        /// Character to send.
        value: char,
    },
    /// Function key F1 through F12.
    Function {
        /// Function-key number.
        number: u8,
    },
}

/// One input event sent to a terminal session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    /// Literal text insertion.
    Text {
        /// Text bytes to write as UTF-8.
        text: String,
    },
    /// A typed key plus held modifiers.
    Key {
        /// Key identity.
        key: KeyCode,
        /// Held modifiers.
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
}

pub(crate) fn encode_events(events: &[InputEvent]) -> Result<Vec<u8>, TerminalError> {
    let mut output = Vec::new();
    for event in events {
        let bytes = match event {
            InputEvent::Text { text } => text.as_bytes().to_vec(),
            InputEvent::Key { key, modifiers } => encode_key(key, modifiers)?,
        };
        if output.len().saturating_add(bytes.len()) > MAX_INPUT_BYTES {
            return Err(TerminalError::InvalidArgument(format!(
                "encoded input exceeds {MAX_INPUT_BYTES} bytes"
            )));
        }
        output.extend_from_slice(&bytes);
    }
    Ok(output)
}

fn encode_key(key: &KeyCode, modifiers: &[Modifier]) -> Result<Vec<u8>, TerminalError> {
    validate_modifiers(modifiers)?;
    match key {
        KeyCode::Character { value } => encode_character(*value, modifiers),
        KeyCode::Function { number } => {
            if !(1..=12).contains(number) {
                return Err(TerminalError::InvalidArgument(
                    "function key number must be between 1 and 12".to_string(),
                ));
            }
            encode_function(*number, modifiers)
        }
        KeyCode::Named { name } => encode_named(*name, modifiers),
    }
}

fn validate_modifiers(modifiers: &[Modifier]) -> Result<(), TerminalError> {
    for (index, modifier) in modifiers.iter().enumerate() {
        if modifiers[..index].contains(modifier) {
            return Err(TerminalError::InvalidArgument(format!(
                "duplicate modifier {modifier:?}"
            )));
        }
    }
    Ok(())
}

fn encode_character(value: char, modifiers: &[Modifier]) -> Result<Vec<u8>, TerminalError> {
    let shift = modifiers.contains(&Modifier::Shift);
    let alt = modifiers.contains(&Modifier::Alt);
    let ctrl = modifiers.contains(&Modifier::Ctrl);
    let super_key = modifiers.contains(&Modifier::Super);
    if super_key {
        return Err(TerminalError::InvalidArgument(
            "super-modified characters have no portable terminal encoding".to_string(),
        ));
    }
    let value = if shift && value.is_ascii_lowercase() {
        value.to_ascii_uppercase()
    } else {
        value
    };
    let encoded = if ctrl {
        let byte = match value.to_ascii_uppercase() {
            '@' | ' ' => 0,
            'A'..='Z' => (value.to_ascii_uppercase() as u8) - b'@',
            '[' => 27,
            '\\' => 28,
            ']' => 29,
            '^' => 30,
            '_' => 31,
            '?' => 127,
            _ => {
                return Err(TerminalError::InvalidArgument(format!(
                    "character {value:?} has no portable control-key encoding"
                )));
            }
        };
        vec![byte]
    } else {
        value.to_string().into_bytes()
    };
    if alt {
        let mut with_escape = Vec::with_capacity(encoded.len() + 1);
        with_escape.push(0x1b);
        with_escape.extend(encoded);
        Ok(with_escape)
    } else {
        Ok(encoded)
    }
}

fn encode_named(name: NamedKey, modifiers: &[Modifier]) -> Result<Vec<u8>, TerminalError> {
    let modifier = xterm_modifier(modifiers);
    let unsupported_simple = modifiers.contains(&Modifier::Super);
    let bytes = match name {
        NamedKey::Up => csi_final('A', modifier),
        NamedKey::Down => csi_final('B', modifier),
        NamedKey::Right => csi_final('C', modifier),
        NamedKey::Left => csi_final('D', modifier),
        NamedKey::Home => csi_final('H', modifier),
        NamedKey::End => csi_final('F', modifier),
        NamedKey::Insert => csi_tilde(2, modifier),
        NamedKey::Delete => csi_tilde(3, modifier),
        NamedKey::PageUp => csi_tilde(5, modifier),
        NamedKey::PageDown => csi_tilde(6, modifier),
        NamedKey::Tab if modifiers == [Modifier::Shift] => b"\x1b[Z".to_vec(),
        NamedKey::Enter | NamedKey::Escape | NamedKey::Tab | NamedKey::Backspace
            if modifiers.is_empty() =>
        {
            match name {
                NamedKey::Enter => b"\r".to_vec(),
                NamedKey::Escape => b"\x1b".to_vec(),
                NamedKey::Tab => b"\t".to_vec(),
                NamedKey::Backspace => b"\x7f".to_vec(),
                _ => unreachable!(),
            }
        }
        NamedKey::Enter | NamedKey::Escape | NamedKey::Tab | NamedKey::Backspace => {
            if unsupported_simple {
                return Err(TerminalError::InvalidArgument(format!(
                    "super-modified {name:?} has no portable terminal encoding"
                )));
            }
            let base = match name {
                NamedKey::Enter => b'\r',
                NamedKey::Escape => 0x1b,
                NamedKey::Tab => b'\t',
                NamedKey::Backspace => 0x7f,
                _ => unreachable!(),
            };
            if modifiers == [Modifier::Alt] {
                vec![0x1b, base]
            } else {
                return Err(TerminalError::InvalidArgument(format!(
                    "modifier combination is unsupported for {name:?}"
                )));
            }
        }
    };
    Ok(bytes)
}

fn encode_function(number: u8, modifiers: &[Modifier]) -> Result<Vec<u8>, TerminalError> {
    let modifier = xterm_modifier(modifiers);
    let code = match number {
        1 => return Ok(csi_final('P', modifier)),
        2 => return Ok(csi_final('Q', modifier)),
        3 => return Ok(csi_final('R', modifier)),
        4 => return Ok(csi_final('S', modifier)),
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => unreachable!(),
    };
    Ok(csi_tilde(code, modifier))
}

fn xterm_modifier(modifiers: &[Modifier]) -> u8 {
    1 + u8::from(modifiers.contains(&Modifier::Shift))
        + 2 * u8::from(modifiers.contains(&Modifier::Alt))
        + 4 * u8::from(modifiers.contains(&Modifier::Ctrl))
        + 8 * u8::from(modifiers.contains(&Modifier::Super))
}

fn csi_final(final_byte: char, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{modifier}{final_byte}").into_bytes()
    }
}

fn csi_tilde(code: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{modifier}~").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_control_and_modified_navigation() {
        assert_eq!(
            encode_key(&KeyCode::Character { value: 'w' }, &[Modifier::Ctrl]).unwrap(),
            vec![0x17]
        );
        assert_eq!(
            encode_key(
                &KeyCode::Named {
                    name: NamedKey::Left
                },
                &[Modifier::Ctrl, Modifier::Shift]
            )
            .unwrap(),
            b"\x1b[1;6D"
        );
    }

    #[test]
    fn validates_the_whole_input_batch_before_use() {
        let err = encode_events(&[
            InputEvent::Text { text: "ok".into() },
            InputEvent::Key {
                key: KeyCode::Function { number: 13 },
                modifiers: vec![],
            },
        ])
        .unwrap_err();
        assert!(err.to_string().contains("between 1 and 12"));
    }
}
