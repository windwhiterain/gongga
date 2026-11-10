//! Reading the console the way a terminal sends it: bytes, not records.
//!
//! # Invariant
//!
//! One reader owns the input queue while this runs. Virtual terminal input makes
//! the console hand out the escape sequences a terminal would, which is the only
//! way `ESC[200~` reaches this window at all; it also takes the queue away from the
//! record reader used before. See `docs/notes/tui-paste.md`.

use crossterm::event::Event;

/// Whether this window reads bytes instead of console records.
///
/// Off until a terminal has proved it: the switch exists so a keyboard that stops
/// working is one restart away from the reader that worked.
pub fn wanted() -> bool {
    std::env::var("GONGGA_TUI_VT_INPUT").is_ok_and(|value| value == "1")
}

#[cfg(windows)]
pub use platform::spawn;

/// No byte reader on a terminal that already sends paste as one string.
#[cfg(not(windows))]
pub fn spawn() -> tokio::sync::mpsc::UnboundedReceiver<Event> {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    rx
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use std::io::{Read, Write};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };

    /// `ENABLE_VIRTUAL_TERMINAL_INPUT`: read the console as a byte stream.
    const VT_INPUT: u32 = 0x0200;
    /// What closes a pasted block. Eight bytes, so the search is a slice compare.
    const PASTE_END: &[u8] = b"\x1b[201~";

    pub fn spawn() -> tokio::sync::mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            if !enable() {
                return;
            }
            let mut reader = std::io::stdin();
            let mut buffer: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = match reader.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(read) => read,
                    Err(_) => return,
                };
                buffer.extend_from_slice(&chunk[..read]);
                log_raw(&chunk[..read]);
                for event in parse(&mut buffer) {
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        rx
    }

    fn enable() -> bool {
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode: u32 = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return false;
            }
            SetConsoleMode(handle, mode | VT_INPUT) != 0
        }
    }

    /// Every event the bytes already hold, leaving an unfinished tail in `buffer`.
    fn parse(buffer: &mut Vec<u8>) -> Vec<Event> {
        let mut events = Vec::new();
        let mut at = 0;
        while at < buffer.len() {
            let byte = buffer[at];
            if byte == 0x1b {
                let Some(next) = buffer.get(at + 1).copied() else {
                    // A lone escape is the key, not the start of a sequence: a
                    // terminal writes a sequence in one go. See the note.
                    events.push(key(KeyCode::Esc, KeyModifiers::NONE));
                    at += 1;
                    continue;
                };
                if next == b'[' {
                    let Some(end) = csi_end(buffer, at + 2) else {
                        break;
                    };
                    let body = buffer[at + 2..end].to_vec();
                    if body == b"200" && buffer[end] == b'~' {
                        let Some(close) = find(&buffer[end + 1..], PASTE_END) else {
                            break;
                        };
                        let start = end + 1;
                        let pasted =
                            String::from_utf8_lossy(&buffer[start..start + close]).to_string();
                        events.push(Event::Paste(pasted));
                        at = start + close + PASTE_END.len();
                        continue;
                    }
                    if let Some(event) = csi(&body, buffer[end]) {
                        events.push(event);
                    }
                    at = end + 1;
                    continue;
                }
                if next == b'O' {
                    let Some(code) = buffer.get(at + 2).copied() else {
                        break;
                    };
                    let plain = KeyModifiers::NONE;
                    match code {
                        b'A' => events.push(key(KeyCode::Up, plain)),
                        b'B' => events.push(key(KeyCode::Down, plain)),
                        b'C' => events.push(key(KeyCode::Right, plain)),
                        b'D' => events.push(key(KeyCode::Left, plain)),
                        b'H' => events.push(key(KeyCode::Home, plain)),
                        b'F' => events.push(key(KeyCode::End, plain)),
                        _ => {}
                    }
                    at += 3;
                    continue;
                }
                let Some((ch, used)) = utf8(&buffer[at + 1..]) else {
                    break;
                };
                events.push(key(KeyCode::Char(ch), KeyModifiers::ALT));
                at += 1 + used;
                continue;
            }
            let event = match byte {
                b'\r' | b'\n' => key(KeyCode::Enter, KeyModifiers::NONE),
                b'\t' => key(KeyCode::Tab, KeyModifiers::NONE),
                0x7f | 0x08 => key(KeyCode::Backspace, KeyModifiers::NONE),
                0x01..=0x1a => key(
                    KeyCode::Char((b'a' + byte - 1) as char),
                    KeyModifiers::CONTROL,
                ),
                _ => {
                    let Some((ch, used)) = utf8(&buffer[at..]) else {
                        break;
                    };
                    events.push(key(KeyCode::Char(ch), KeyModifiers::NONE));
                    at += used;
                    continue;
                }
            };
            events.push(event);
            at += 1;
        }
        buffer.drain(..at);
        events
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Where the sequence whose parameters start at `at` ends: its final byte.
    fn csi_end(buffer: &[u8], at: usize) -> Option<usize> {
        buffer
            .iter()
            .enumerate()
            .skip(at)
            .find(|(_, byte)| (0x40..=0x7e).contains(*byte))
            .map(|(index, _)| index)
    }

    /// One CSI sequence whose body is `body` and whose last byte is `last`.
    fn csi(body: &[u8], last: u8) -> Option<Event> {
        if body.first() == Some(&b'<') {
            return sgr_mouse(&body[1..], last);
        }
        let numbers = numbers_of(body);
        let modifiers = modifiers_of(numbers.get(1).copied());
        let code = match (last, numbers.first().copied()) {
            (b'A', _) => KeyCode::Up,
            (b'B', _) => KeyCode::Down,
            (b'C', _) => KeyCode::Right,
            (b'D', _) => KeyCode::Left,
            (b'H', _) => KeyCode::Home,
            (b'F', _) => KeyCode::End,
            (b'Z', _) => KeyCode::BackTab,
            (b'~', Some(1 | 7)) => KeyCode::Home,
            (b'~', Some(2)) => KeyCode::Insert,
            (b'~', Some(3)) => KeyCode::Delete,
            (b'~', Some(4 | 8)) => KeyCode::End,
            (b'~', Some(5)) => KeyCode::PageUp,
            (b'~', Some(6)) => KeyCode::PageDown,
            _ => return None,
        };
        Some(key(code, modifiers))
    }

    fn numbers_of(body: &[u8]) -> Vec<u32> {
        body.split(|byte| *byte == b';')
            .filter(|part| !part.is_empty())
            .filter_map(|part| std::str::from_utf8(part).ok()?.parse().ok())
            .collect()
    }

    /// SGR mouse: `b ; column ; row` then `M` for a press and `m` for a release.
    fn sgr_mouse(body: &[u8], last: u8) -> Option<Event> {
        let numbers = numbers_of(body);
        let button = *numbers.first()?;
        let column = u16::try_from(*numbers.get(1)?).ok()?.saturating_sub(1);
        let row = u16::try_from(*numbers.get(2)?).ok()?.saturating_sub(1);
        let modifiers = modifiers_of(Some((button & !0b11000011) >> 2));
        let kind = match (button & 0b1100_0011, last) {
            (64, _) => MouseEventKind::ScrollUp,
            (65, _) => MouseEventKind::ScrollDown,
            (0 | 32, _) if last == b'm' => MouseEventKind::Up(MouseButton::Left),
            (0 | 32, _) => MouseEventKind::Down(MouseButton::Left),
            (1 | 33, _) if last == b'm' => MouseEventKind::Up(MouseButton::Middle),
            (1 | 33, _) => MouseEventKind::Down(MouseButton::Middle),
            (2 | 34, _) if last == b'm' => MouseEventKind::Up(MouseButton::Right),
            (2 | 34, _) => MouseEventKind::Down(MouseButton::Right),
            _ => return None,
        };
        Some(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }))
    }

    fn modifiers_of(parameter: Option<u32>) -> KeyModifiers {
        match parameter.unwrap_or(1).saturating_sub(1) {
            0 => KeyModifiers::NONE,
            1 => KeyModifiers::SHIFT,
            2 => KeyModifiers::ALT,
            3 => KeyModifiers::SHIFT | KeyModifiers::ALT,
            4 => KeyModifiers::CONTROL,
            5 => KeyModifiers::SHIFT | KeyModifiers::CONTROL,
            6 => KeyModifiers::ALT | KeyModifiers::CONTROL,
            _ => KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
        }
    }

    /// One character and how many bytes it took, or `None` when it is not whole.
    fn utf8(bytes: &[u8]) -> Option<(char, usize)> {
        for end in 1..=bytes.len().min(4) {
            if let Ok(text) = std::str::from_utf8(&bytes[..end]) {
                if let Some(ch) = text.chars().next() {
                    return Some((ch, end));
                }
            }
        }
        None
    }
    fn log_raw(bytes: &[u8]) {
        let Some(path) = std::env::var_os("GONGGA_TUI_RAW_KEYS") else {
            return;
        };
        let mut line = String::with_capacity(bytes.len() * 3 + 1);
        for byte in bytes {
            line.push_str(&format!("{byte:02x} "));
        }
        line.push('\n');
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn events(bytes: &[u8]) -> Vec<Event> {
            let mut buffer = bytes.to_vec();
            parse(&mut buffer)
        }

        #[test]
        fn a_pasted_block_arrives_as_one_event() {
            let got = events(b"\x1b[200~line one\r\nline two\x1b[201~");
            assert_eq!(got.len(), 1, "{got:?}");
            match &got[0] {
                Event::Paste(text) => assert_eq!(text, "line one\r\nline two"),
                other => panic!("not a paste: {other:?}"),
            }
        }

        #[test]
        fn the_keys_a_hand_types_are_keys() {
            assert!(matches!(events(b"a")[0], Event::Key(key) if key.code == KeyCode::Char('a')));
            assert!(matches!(events(b"\r")[0], Event::Key(key) if key.code == KeyCode::Enter));
            assert!(matches!(events(b"\x1b[A")[0], Event::Key(key) if key.code == KeyCode::Up));
            let control = events(b"\x1b[1;5A");
            assert!(
                matches!(&control[0], Event::Key(key) if key.code == KeyCode::Up
                    && key.modifiers.contains(KeyModifiers::CONTROL)),
                "{control:?}"
            );
            assert!(
                matches!(events(b"\x03")[0], Event::Key(key)
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
            );
            assert!(matches!(events(b"\x1b[3~")[0], Event::Key(key) if key.code == KeyCode::Delete));
        }

        #[test]
        fn half_a_sequence_waits_for_the_rest() {
            let mut buffer = b"\x1b[20".to_vec();
            assert!(parse(&mut buffer).is_empty(), "{buffer:?}");
            assert_eq!(&buffer[..], b"\x1b[20");
            buffer.extend_from_slice(b"0~pasted\x1b[201~");
            let got = parse(&mut buffer);
            assert!(
                matches!(&got[0], Event::Paste(text) if text == "pasted"),
                "{got:?}"
            );
        }

        #[test]
        fn a_click_names_its_cell() {
            let got = events(b"\x1b[<0;10;20M");
            match &got[0] {
                Event::Mouse(mouse) => {
                    assert_eq!((mouse.column, mouse.row), (9, 19));
                    assert!(matches!(
                        mouse.kind,
                        MouseEventKind::Down(MouseButton::Left)
                    ));
                }
                other => panic!("not a mouse event: {other:?}"),
            }
        }

        #[test]
        fn a_lone_escape_is_the_key() {
            let got = events(b"\x1b");
            assert!(
                matches!(got[0], Event::Key(key) if key.code == KeyCode::Esc),
                "{got:?}"
            );
        }
    }
}
