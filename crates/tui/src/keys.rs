//! Teclado do crossterm → bytes de terminal. Fica na TUI: o núcleo recebe
//! bytes prontos, e o app desktop recebe os dele do xterm.js.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Converte um `KeyEvent` do crossterm nos bytes que um terminal envia.
///
/// Retorna `None` para teclas que não têm representação (ou que a TUI
/// reserva para si e não devem chegar aqui).
pub fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let mut out: Vec<u8> = Vec::new();
    if alt {
        out.push(0x1b); // prefixo ESC para Alt+tecla
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Z → 0x01..0x1A
                let c = c.to_ascii_lowercase();
                if c.is_ascii_lowercase() {
                    out.push((c as u8) - b'a' + 1);
                } else {
                    return None;
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n @ 1..=4) => {
            out.extend_from_slice(b"\x1bO");
            out.push(b'P' + (n - 1));
        }
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_chars_and_enter() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap(),
            b"a"
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            b"\r"
        );
        // UTF-8 multi-byte
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('ç'), KeyModifiers::NONE)).unwrap(),
            "ç".as_bytes()
        );
    }

    #[test]
    fn ctrl_and_alt_combos() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)).unwrap(),
            vec![0x03]
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('b'), KeyModifiers::ALT)).unwrap(),
            vec![0x1b, b'b']
        );
    }

    #[test]
    fn arrows() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
            b"\x1b[A"
        );
    }
}
