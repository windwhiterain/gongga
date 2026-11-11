//! Reading the console the way a terminal sends it: bytes, not records.
//!
//! # Invariant
//!
//! One reader owns the input queue while this runs. On Windows, virtual terminal
//! input makes the console hand out the escape sequences a terminal would -- the
//! only way `ESC[200~` reaches this window at all. Elsewhere crossterm's reader
//! already reports a paste as one string. See `docs/notes/tui-paste.md`.

use crossterm::event::Event;

/// The one reader of terminal events this window opens.
pub struct Input {
    #[cfg(windows)]
    reader: tokio::sync::mpsc::UnboundedReceiver<Event>,
    #[cfg(not(windows))]
    reader: crossterm::event::EventStream,
}

#[cfg(windows)]
impl Input {
    pub fn open() -> Self {
        Self {
            reader: platform::spawn(),
        }
    }

    pub async fn next(&mut self) -> Option<std::io::Result<Event>> {
        self.reader.recv().await.map(Ok)
    }
}

#[cfg(not(windows))]
impl Input {
    pub fn open() -> Self {
        Self {
            reader: crossterm::event::EventStream::new(),
        }
    }

    pub async fn next(&mut self) -> Option<std::io::Result<Event>> {
        use futures_util::StreamExt;
        self.reader.next().await
    }
}

/// A Windows console's mode: elsewhere a key arrives with its modifiers.
#[cfg(not(windows))]
pub fn request_win32_input(_out: &mut impl std::io::Write) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn release_win32_input(_out: &mut impl std::io::Write) -> std::io::Result<()> {
    Ok(())
}
#[cfg(windows)]
pub use platform::{release_win32_input, request_win32_input};

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
    /// `DECSET 9001`: one record per key event, modifiers included.
    ///
    /// # Invariant
    ///
    /// The console's VT translation has no shape for a modified `Enter`: it
    /// writes a plain `CR`, so `Shift+Enter` would submit what the operator
    /// meant to break. This mode is the console's own answer about the key --
    /// `Vk;Sc;Uc;Kd;Cs;Rc` between `[` and `_` -- and a terminal that does not
    /// know it ignores it, leaving the bytes the parser already reads.
    /// See `docs/notes/tui-paste.md`.
    const WIN32_INPUT_ON: &[u8] = b"\x1b[?9001h";
    /// The same mode off, so the shell this window returns to reads its keys.

    const WIN32_INPUT_OFF: &[u8] = b"\x1b[?9001l";
    /// How long an escape waits for the rest of its sequence before it is a key.
    ///
    /// # Invariant
    ///
    /// A console writes one gesture in more than one batch, so the byte that ends
    /// a read may be the first of a sequence: `ESC` then `[<64;50;24M` is one
    /// wheel turn, not a key and then text. Longer than a write takes, shorter
    /// than a hand waits. See `docs/notes/tui-paste.md`.
    const ESCAPE_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

    pub fn spawn() -> tokio::sync::mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            if !enable() {
                return;
            }
            // Reading blocks, so it has its own thread: the parser must be free
            // to decide the rest is not coming.
            let (arrived, bytes) = std::sync::mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let mut reader = std::io::stdin();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => read,
                    };
                    if arrived.send(chunk[..read].to_vec()).is_err() {
                        return;
                    }
                }
            });
            let mut log = log_file();
            let mut buffer: Vec<u8> = Vec::new();
            // The other half of a character outside the BMP, if one is held.
            let mut pending: Option<u16> = None;
            loop {
                // Only a batch that left bytes behind has a reason to time out.
                let batch = if buffer.is_empty() {
                    bytes.recv().ok()
                } else {
                    bytes.recv_timeout(ESCAPE_WAIT).ok()
                };
                let Some(batch) = batch else {
                    if buffer.is_empty() {
                        return;
                    }
                    for event in flush(&mut buffer) {
                        log_event(&mut log, &event);
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    continue;
                };
                log_raw(&mut log, &batch);
                let held = buffer.len();
                buffer.extend_from_slice(&batch);
                // A record is whole: it never finishes an escape an earlier batch
                // left open. See `docs/notes/tui-paste.md`.
                if held > 0 && starts_a_record(&buffer[held..]) {
                    buffer.drain(..held);
                }
                for event in parse(&mut buffer, &mut pending) {
                    log_event(&mut log, &event);
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        });
        rx
    }

    /// Ends a sequence the console never finished: a lone escape is the key.
    ///
    /// # Invariant
    ///
    /// The rest of an unfinished sequence is dropped, never typed: the bytes of
    /// `ESC[<64;50;24M` are a wheel turn, and a prompt holding them is not a
    /// message anyone wrote.
    fn flush(buffer: &mut Vec<u8>) -> Vec<Event> {
        let alone = buffer.first() == Some(&0x1b);
        buffer.clear();
        alone
            .then(|| key(KeyCode::Esc, KeyModifiers::NONE))
            .into_iter()
            .collect()
    }

    /// Asks the terminal for Win32 input mode.
    pub fn request_win32_input(out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(WIN32_INPUT_ON)?;
        out.flush()
    }

    /// Gives the mode back.
    pub fn release_win32_input(out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(WIN32_INPUT_OFF)?;
        out.flush()
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
    fn parse(buffer: &mut Vec<u8>, pending: &mut Option<u16>) -> Vec<Event> {
        let mut events = Vec::new();
        let mut at = 0;
        while at < buffer.len() {
            let byte = buffer[at];
            if byte == 0x1b {
                let Some(next) = buffer.get(at + 1).copied() else {
                    // Held, not taken: the rest may be the next batch, and only
                    // silence says the escape was the key. See `flush`.
                    break;
                };
                if let Some((after, decoded)) = record_at(buffer, at, pending) {
                    match decoded {
                        Decoded::Key(event) => events.push(event),
                        Decoded::Nothing => {}
                        // A run of byte-carrying records is the stream a program
                        // wrote, put back where it was. See `docs/notes/tui-paste.md`.
                        Decoded::Byte(unit) => {
                            let mut written = Vec::new();
                            push_utf8(&mut written, unit);
                            let mut next = after;
                            while let Some((then, Decoded::Byte(unit))) =
                                record_at(buffer, next, pending)
                            {
                                push_utf8(&mut written, unit);
                                next = then;
                            }
                            buffer.splice(at..next, written);
                            continue;
                        }
                    }
                    at = after;
                    continue;
                }
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
                // A control byte after the escape is a key, not a character: an
                // escape then a carriage return is Alt+Enter, and typing the raw
                // byte would put a newline inside the prompt instead of sending.
                match buffer.get(at + 1).copied() {
                    Some(b'\r' | b'\n') => {
                        events.push(key(KeyCode::Enter, KeyModifiers::ALT));
                        at += 2;
                        continue;
                    }
                    Some(b'\t') => {
                        events.push(key(KeyCode::Tab, KeyModifiers::ALT));
                        at += 2;
                        continue;
                    }
                    _ => {}
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
        // The button word keeps its modifiers one code below the parameter
        // `modifiers_of` reads.
        let modifiers = modifiers_of(Some(((button & 0b0001_1100) >> 2) + 1));
        // Bit 5 is motion, so a drag and a click differ by that bit alone: a
        // handler that only knows clicks loses the gesture that selects text.
        let held = button & 0b11;
        let kind = match (button & 0b1110_0011, last) {
            (64, _) => MouseEventKind::ScrollUp,
            (65, _) => MouseEventKind::ScrollDown,
            (value, b'M') if value & 32 != 0 => match held {
                0 => MouseEventKind::Drag(MouseButton::Left),
                1 => MouseEventKind::Drag(MouseButton::Middle),
                2 => MouseEventKind::Drag(MouseButton::Right),
                _ => MouseEventKind::Moved,
            },
            (0, _) if last == b'm' => MouseEventKind::Up(MouseButton::Left),
            (0, _) => MouseEventKind::Down(MouseButton::Left),
            (1, _) if last == b'm' => MouseEventKind::Up(MouseButton::Middle),
            (1, _) => MouseEventKind::Down(MouseButton::Middle),
            (2, _) if last == b'm' => MouseEventKind::Up(MouseButton::Right),
            (2, _) => MouseEventKind::Down(MouseButton::Right),
            _ => return None,
        };
        Some(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }))
    }

    /// `dwControlKeyState` bits of a Win32 key event: which modifiers are held.
    const SHIFT_HELD: u32 = 0x0010;
    const LEFT_ALT: u32 = 0x0002;
    const RIGHT_ALT: u32 = 0x0001;
    const LEFT_CTRL: u32 = 0x0008;
    const RIGHT_CTRL: u32 = 0x0004;

    /// What one `Vk;Sc;Uc;Kd;Cs;Rc` record says.
    enum Decoded {
        /// A key: named by its virtual-key code, or a character carrying modifiers.
        Key(Event),
        /// A character a program wrote to the console rather than a key a person
        /// pressed: the byte it is, not the key it is not.
        Byte(u32),
        /// Neither: a release, a modifier on its own, or half a character.
        Nothing,
    }

    /// One record: the key it names, on its way down only.
    ///
    /// # Invariant
    ///
    /// The modifiers are read from the record and never guessed from the
    /// character, because the key this exists for has no character at all. A
    /// release is not a key this window acts on, a modifier on its own is no
    /// key, and a key that is neither in the table nor carrying a character is
    /// no event rather than a guess. See `docs/notes/tui-paste.md`.
    fn record(body: &[u8], pending: &mut Option<u16>) -> Decoded {
        // Positional, not `numbers_of`: field four is where the state is.
        let fields: Vec<u32> = body
            .split(|byte| *byte == b';')
            .map(|part| {
                std::str::from_utf8(part)
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .unwrap_or(0)
            })
            .collect();
        if fields.get(3).copied().unwrap_or(1) == 0 {
            return Decoded::Nothing;
        }
        let modifiers = held(fields.get(4).copied().unwrap_or(0));
        let vk = fields.first().copied().unwrap_or(0);
        if let Some(code) = name(vk) {
            return Decoded::Key(key(code, modifiers));
        }
        let Some(character) = character(fields.get(2).copied().unwrap_or(0), pending, modifiers)
        else {
            return Decoded::Nothing;
        };
        // No virtual-key code: nothing was pressed, bytes were written.
        if vk == 0 {
            return Decoded::Byte(u32::from(character));
        }
        Decoded::Key(key(KeyCode::Char(character), modifiers))
    }

    fn held(state: u32) -> KeyModifiers {
        let mut modifiers = KeyModifiers::NONE;
        if state & SHIFT_HELD != 0 {
            modifiers |= KeyModifiers::SHIFT;
        }
        if state & (LEFT_ALT | RIGHT_ALT) != 0 {
            modifiers |= KeyModifiers::ALT;
        }
        if state & (LEFT_CTRL | RIGHT_CTRL) != 0 {
            modifiers |= KeyModifiers::CONTROL;
        }
        modifiers
    }

    /// The key a virtual-key code names, for the codes whose character says nothing.
    fn name(vk: u32) -> Option<KeyCode> {
        let code = match vk {
            0x0D => KeyCode::Enter,
            0x1B => KeyCode::Esc,
            0x09 => KeyCode::Tab,
            0x08 => KeyCode::Backspace,
            0x2D => KeyCode::Insert,
            0x2E => KeyCode::Delete,
            0x25 => KeyCode::Left,
            0x26 => KeyCode::Up,
            0x27 => KeyCode::Right,
            0x28 => KeyCode::Down,
            0x24 => KeyCode::Home,
            0x23 => KeyCode::End,
            0x21 => KeyCode::PageUp,
            0x22 => KeyCode::PageDown,
            0x70..=0x73 => KeyCode::F((vk - 0x6F) as u8),
            _ => return None,
        };
        Some(code)
    }

    /// The character a record carries, if it carries one.
    ///
    /// # Invariant
    ///
    /// A unit of zero is no character at all -- a modifier on its own, or a key
    /// whose name the table holds. A control byte is the letter the console
    /// means rather than the byte, and a character outside the BMP comes as two
    /// records: the first half is held until its other half arrives, so one key
    /// is one character and never two.
    fn character(unit: u32, pending: &mut Option<u16>, modifiers: KeyModifiers) -> Option<char> {
        let unit = u16::try_from(unit).ok()?;
        if unit == 0 {
            return None;
        }
        if (0xD800..=0xDBFF).contains(&unit) {
            *pending = Some(unit);
            return None;
        }
        let half = pending.take();
        if let (0xDC00..=0xDFFF, Some(high)) = (unit, half) {
            let value = 0x10000 + (u32::from(high) - 0xD800) * 0x400 + (u32::from(unit) - 0xDC00);
            return char::from_u32(value);
        }
        if (1..=0x1A).contains(&unit) && modifiers.contains(KeyModifiers::CONTROL) {
            return Some(char::from(b'a' + (unit as u8) - 1));
        }
        char::from_u32(u32::from(unit))
    }

    /// Whether the bytes start with a whole record: `ESC [ Vk ; ... _`.
    fn starts_a_record(buffer: &[u8]) -> bool {
        if buffer.first() != Some(&0x1b) || buffer.get(1) != Some(&b'[') {
            return false;
        }
        match csi_end(buffer, 2) {
            Some(end) => buffer.get(end) == Some(&b'_'),
            None => false,
        }
    }

    /// The record starting at `at`, if the bytes there are one.
    fn record_at(buffer: &[u8], at: usize, pending: &mut Option<u16>) -> Option<(usize, Decoded)> {
        if buffer.get(at) != Some(&0x1b) || buffer.get(at + 1) != Some(&b'[') {
            return None;
        }
        let end = csi_end(buffer, at + 2)?;
        if buffer.get(end) != Some(&b'_') {
            return None;
        }
        Some((end + 1, record(&buffer[at + 2..end], pending)))
    }

    /// The bytes of a character, so a record can be put back into the stream.
    fn push_utf8(out: &mut Vec<u8>, unit: u32) {
        let mut buffer = [0u8; 4];
        if let Some(text) = char::from_u32(unit).map(|character| character.encode_utf8(&mut buffer))
        {
            out.extend_from_slice(text.as_bytes());
        }
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
    /// The file the raw bytes go to: `GONGGA_TUI_RAW_KEYS`, else one in the temp
    /// directory, made fresh each run so one run is one file.
    ///
    /// # Invariant
    ///
    /// Reading what the console delivered is never something a person has to
    /// configure. See `docs/notes/tui-paste.md`.
    fn log_file() -> Option<std::fs::File> {
        let path = std::env::var_os("GONGGA_TUI_RAW_KEYS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("gongga-raw-keys.log"));
        std::fs::File::create(path).ok()
    }

    fn log_raw(file: &mut Option<std::fs::File>, bytes: &[u8]) {
        let Some(file) = file else {
            return;
        };
        let mut line = String::with_capacity(bytes.len() * 3 + 1);
        for byte in bytes {
            line.push_str(&format!("{byte:02x} "));
        }
        line.push('\n');
        let _ = file.write_all(line.as_bytes());
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn events(bytes: &[u8]) -> Vec<Event> {
            let mut buffer = bytes.to_vec();
            let mut pending = None;
            parse(&mut buffer, &mut pending)
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
            assert!(matches!(events(b"\x03")[0], Event::Key(key)
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)));
            assert!(
                matches!(events(b"\x1b[3~")[0], Event::Key(key) if key.code == KeyCode::Delete)
            );
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
            let mut buffer = b"\x1b".to_vec();
            assert!(
                parse(&mut buffer).is_empty(),
                "an escape waits for its rest"
            );
            let got = flush(&mut buffer);
            assert!(
                matches!(got[0], Event::Key(key) if key.code == KeyCode::Esc),
                "{got:?}"
            );
            assert!(buffer.is_empty(), "{buffer:?}");
        }

        #[test]
        fn a_wheel_turn_split_across_batches_is_still_a_wheel() {
            let mut buffer = b"\x1b".to_vec();
            assert!(parse(&mut buffer).is_empty(), "{buffer:?}");
            buffer.extend_from_slice(b"[<64;50;24M");
            let got = parse(&mut buffer);
            assert!(
                matches!(&got[0], Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp),
                "{got:?}"
            );
            assert!(buffer.is_empty(), "{buffer:?}");
        }

        #[test]
        fn an_unfinished_sequence_never_becomes_text() {
            let mut buffer = b"\x1b[<64".to_vec();
            assert!(parse(&mut buffer).is_empty(), "{buffer:?}");
            let got = flush(&mut buffer);
            assert_eq!(got.len(), 1, "{got:?}");
            assert!(
                matches!(got[0], Event::Key(key) if key.code == KeyCode::Esc),
                "{got:?}"
            );
        }
    }
    fn log_event(file: &mut Option<std::fs::File>, event: &Event) {
        let Some(file) = file else {
            return;
        };
        let line = format!("E {event:?}\n");
        let _ = file.write_all(line.as_bytes());
    }
}
