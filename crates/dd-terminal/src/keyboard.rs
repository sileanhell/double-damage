//! Session-scoped keyboard terminal protocols, like rho's keyboard_modes.
//!
//! Shift+Enter and other modified keys are only reported when the terminal
//! encodes them: the kitty keyboard protocol or xterm modifyOtherKeys mode 2.
//! Windows ConPTY reverse-maps only a small set of legacy key sequences into
//! console records, so the extra fidelity those protocols provide is requested
//! everywhere except Windows — the same trade-off rho makes.
//!
//! Bracketed paste is armed unconditionally: terminals that honor `\x1b[?2004h`
//! wrap pasted text in markers, and crossterm's unix parser surfaces it as
//! `Event::Paste`. On Windows ConPTY simply discards the marker sequences, so
//! arming it there is harmless — and ready if ConPTY ever forwards them.

use std::io::{self, Write};

use ratatui::crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};

/// xterm modifyOtherKeys mode 2: report modified function keys (including
/// modified Enter) without altering ordinary shifted printable characters.
const ENABLE_MODIFY_OTHER_KEYS: &[u8] = b"\x1b[>4;2m";
const DISABLE_MODIFY_OTHER_KEYS: &[u8] = b"\x1b[>4;0m";

/// Keyboard protocol guards armed for the TUI lifetime.
///
/// Call [`Self::release`] during shutdown, before `ratatui::restore`, so the
/// terminal stack unwinds in order.
pub(crate) struct KeyboardProtocols {
    modified_keys: bool,
    keyboard_enhancements: bool,
    bracketed_paste: bool,
}

impl KeyboardProtocols {
    /// Request extended key reporting where ConPTY cannot be in the way, and
    /// bracketed paste everywhere — ConPTY drops the markers harmlessly.
    pub(crate) fn acquire() -> Self {
        let extended = !cfg!(windows);
        Self {
            modified_keys: extended && write_sequence(ENABLE_MODIFY_OTHER_KEYS).is_ok(),
            keyboard_enhancements: extended
                && execute!(
                    io::stdout(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )
                .is_ok(),
            bracketed_paste: execute!(io::stdout(), EnableBracketedPaste).is_ok(),
        }
    }

    pub(crate) fn release(self) {
        if self.bracketed_paste {
            let _ = execute!(io::stdout(), DisableBracketedPaste);
        }
        if self.keyboard_enhancements {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        if self.modified_keys {
            let _ = write_sequence(DISABLE_MODIFY_OTHER_KEYS);
        }
    }
}

fn write_sequence(sequence: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(sequence)?;
    stdout.flush()
}
