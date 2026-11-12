#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Char(char),
    Enter,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Escape,
    CtrlC,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    LeftDown,
    Drag,
    LeftUp,
    Move,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

fn parse_sgr_mouse(seq: &str, final_byte: u8) -> Option<MouseEvent> {
    if final_byte != b'M' && final_byte != b'm' {
        return None;
    }
    let mut parts = seq.split(';');
    let btn = parts.next()?.parse::<u16>().ok()?;
    let col = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);
    let row = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);

    if final_byte == b'm' {
        if (btn & 3) == 0 {
            return Some(MouseEvent {
                kind: MouseKind::LeftUp,
                col,
                row,
            });
        }
        return None;
    }

    let kind = if btn & 64 != 0 {
        if btn & 1 == 0 {
            MouseKind::ScrollUp
        } else {
            MouseKind::ScrollDown
        }
    } else if btn & 32 != 0 {
        if (btn & 3) == 0 {
            MouseKind::Drag
        } else {
            MouseKind::Move
        }
    } else if (btn & 3) == 0 {
        MouseKind::LeftDown
    } else {
        return None;
    };

    Some(MouseEvent { kind, col, row })
}

fn parse_csi_sequence(
    params: &[u8],
    final_byte: u8,
    keys: &mut Vec<KeyEvent>,
    mice: &mut Vec<MouseEvent>,
) {
    if params.first() == Some(&b'<') {
        if let Ok(seq) = std::str::from_utf8(&params[1..])
            && let Some(ev) = parse_sgr_mouse(seq, final_byte)
        {
            mice.push(ev);
        }
        return;
    }

    match final_byte {
        b'A' => keys.push(KeyEvent::Up),
        b'B' => keys.push(KeyEvent::Down),
        b'C' => keys.push(KeyEvent::Right),
        b'D' => keys.push(KeyEvent::Left),
        b'Z' => keys.push(KeyEvent::BackTab),
        b'H' => keys.push(KeyEvent::Home),
        b'F' => keys.push(KeyEvent::End),
        b'~' => {
            let code = std::str::from_utf8(params)
                .ok()
                .and_then(|s| s.split(';').next())
                .unwrap_or("");
            match code {
                "1" | "7" => keys.push(KeyEvent::Home),
                "4" | "8" => keys.push(KeyEvent::End),
                "5" => keys.push(KeyEvent::PageUp),
                "6" => keys.push(KeyEvent::PageDown),
                _ => {}
            }
        }
        _ => {}
    }
}

pub fn parse_input_events(bytes: &[u8]) -> (Vec<KeyEvent>, Vec<MouseEvent>) {
    let mut keys = Vec::new();
    let mut mice = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            if i + 1 >= bytes.len() {
                keys.push(KeyEvent::Escape);
                i += 1;
                continue;
            }
            match bytes[i + 1] {
                b'[' => {
                    // CSI sequence: \x1b[ ... <final byte in 0x40..=0x7E>
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j >= bytes.len() {
                        // Incomplete CSI sequence at end of read buffer: ignore safely
                        break;
                    }
                    parse_csi_sequence(&bytes[i + 2..j], bytes[j], &mut keys, &mut mice);
                    i = j + 1;
                    continue;
                }
                b'O' => {
                    // SS3 sequence: \x1bO<final>
                    if i + 2 >= bytes.len() {
                        break;
                    }
                    match bytes[i + 2] {
                        b'A' => keys.push(KeyEvent::Up),
                        b'B' => keys.push(KeyEvent::Down),
                        b'C' => keys.push(KeyEvent::Right),
                        b'D' => keys.push(KeyEvent::Left),
                        b'H' => keys.push(KeyEvent::Home),
                        b'F' => keys.push(KeyEvent::End),
                        _ => {}
                    }
                    i += 3;
                    continue;
                }
                _ => {
                    keys.push(KeyEvent::Escape);
                    i += 1;
                    continue;
                }
            }
        }
        match b {
            0x03 => {
                keys.push(KeyEvent::CtrlC);
                i += 1;
            }
            b'\r' | b'\n' => {
                keys.push(KeyEvent::Enter);
                i += 1;
            }
            b'\t' => {
                keys.push(KeyEvent::Tab);
                i += 1;
            }
            0x7f | 0x08 => {
                keys.push(KeyEvent::Backspace);
                i += 1;
            }
            _ => {
                if let Ok(s) = std::str::from_utf8(&bytes[i..])
                    && let Some(ch) = s.chars().next()
                {
                    keys.push(KeyEvent::Char(ch));
                    i += ch.len_utf8();
                } else {
                    i += 1;
                }
            }
        }
    }
    (keys, mice)
}
