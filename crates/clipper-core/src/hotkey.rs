//! Platform-independent global hotkey model: parsing, display and validation.
//!
//! The canonical text form (`Ctrl+Shift+F10`) is what settings store and the UI
//! shows. Platform backends map [`Key::virtual_key`] and [`Modifiers`] to their
//! own registration API.

use std::fmt;
use std::str::FromStr;

/// Default shortcut for saving the instant replay.
pub const DEFAULT_SAVE_REPLAY: &str = "Ctrl+Shift+F10";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Windows / Super key.
    pub win: bool,
}

impl Modifiers {
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }
}

/// The non-modifier key of a hotkey. Only keys that are stable across keyboard
/// layouts are supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// `A`..=`Z`, stored uppercase.
    Letter(char),
    /// `0`..=`9`.
    Digit(u8),
    /// `F1`..=`F24`.
    Function(u8),
}

impl Key {
    /// Parses a key name such as `a`, `7` or `f10` (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        let upper = name.trim().to_ascii_uppercase();
        let mut chars = upper.chars();
        match (chars.next(), chars.next()) {
            (Some(c @ 'A'..='Z'), None) => Some(Self::Letter(c)),
            (Some(c @ '0'..='9'), None) => Some(Self::Digit(c as u8 - b'0')),
            (Some('F'), Some(_)) => match upper[1..].parse::<u8>() {
                Ok(n @ 1..=24) if !upper[1..].starts_with('0') => Some(Self::Function(n)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Windows virtual-key code.
    pub fn virtual_key(self) -> u16 {
        match self {
            Self::Letter(c) => c as u16,
            Self::Digit(d) => 0x30 + u16::from(d),
            Self::Function(n) => 0x70 + u16::from(n - 1),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Letter(c) => write!(f, "{c}"),
            Self::Digit(d) => write!(f, "{d}"),
            Self::Function(n) => write!(f, "F{n}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: Key,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    /// No text, or only modifiers.
    MissingKey,
    UnknownKey(String),
    /// More than one non-modifier key, e.g. `Ctrl+A+B`.
    TooManyKeys,
    DuplicateModifier(&'static str),
    /// Letters and digits alone (or with Shift only) would block normal typing.
    NeedsModifier,
    /// Windows or Clipper itself already uses this shortcut.
    Reserved(&'static str),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey => f.write_str("Add a letter, number or F-key to the shortcut."),
            Self::UnknownKey(key) => write!(
                f,
                "\"{key}\" is not supported. Use letters, numbers or F1-F24."
            ),
            Self::TooManyKeys => f.write_str("Use one key plus modifiers."),
            Self::DuplicateModifier(name) => write!(f, "{name} appears twice."),
            Self::NeedsModifier => f.write_str(
                "Letters and numbers need Ctrl, Alt or Win, or games could not use them.",
            ),
            Self::Reserved(owner) => write!(f, "This shortcut is reserved by {owner}."),
        }
    }
}

impl std::error::Error for HotkeyError {}

impl Hotkey {
    pub fn new(modifiers: Modifiers, key: Key) -> Result<Self, HotkeyError> {
        let hotkey = Self { modifiers, key };
        hotkey.validate()?;
        Ok(hotkey)
    }

    pub fn default_save_replay() -> Self {
        DEFAULT_SAVE_REPLAY
            .parse()
            .expect("default hotkey is valid")
    }

    /// Rejects combinations that would break typing or collide with Windows or
    /// Clipper's in-window shortcuts. Conflicts with other applications are
    /// only detectable at registration time.
    pub fn validate(&self) -> Result<(), HotkeyError> {
        let m = self.modifiers;
        let only_shift = m.shift && !(m.ctrl || m.alt || m.win);
        if matches!(self.key, Key::Letter(_) | Key::Digit(_)) && (m.is_empty() || only_shift) {
            return Err(HotkeyError::NeedsModifier);
        }
        // Win+key without Ctrl/Alt is almost entirely taken by Windows, and
        // Win+Alt+key is the Xbox Game Bar's capture family.
        if m.win && !m.ctrl {
            return Err(HotkeyError::Reserved("Windows"));
        }
        if self.key == Key::Function(12) && m.is_empty() {
            return Err(HotkeyError::Reserved("Windows (debugger key)"));
        }
        if self.key == Key::Function(4) && m.alt && !(m.ctrl || m.shift || m.win) {
            return Err(HotkeyError::Reserved("Windows (close window)"));
        }
        let ctrl_only = m.ctrl && !(m.alt || m.shift || m.win);
        let clipper_key = matches!(self.key, Key::Digit(1..=4) | Key::Letter('K' | 'Q'));
        if ctrl_only && clipper_key {
            return Err(HotkeyError::Reserved("Clipper"));
        }
        Ok(())
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;

    /// Parses `Ctrl+Shift+F10`. Case-insensitive; accepts `Control`, `Super`
    /// and `Windows` aliases and surrounding spaces.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in text
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (flag, name) = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => (&mut modifiers.ctrl, "Ctrl"),
                "alt" => (&mut modifiers.alt, "Alt"),
                "shift" => (&mut modifiers.shift, "Shift"),
                "win" | "windows" | "super" | "meta" => (&mut modifiers.win, "Win"),
                _ => {
                    let parsed =
                        Key::from_name(part).ok_or_else(|| HotkeyError::UnknownKey(part.into()))?;
                    if key.replace(parsed).is_some() {
                        return Err(HotkeyError::TooManyKeys);
                    }
                    continue;
                }
            };
            if std::mem::replace(flag, true) {
                return Err(HotkeyError::DuplicateModifier(name));
            }
        }
        Self::new(modifiers, key.ok_or(HotkeyError::MissingKey)?)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Hotkey, HotkeyError> {
        text.parse()
    }

    #[test]
    fn default_round_trips() {
        let hotkey = Hotkey::default_save_replay();
        assert_eq!(hotkey.to_string(), DEFAULT_SAVE_REPLAY);
        assert_eq!(hotkey.key.virtual_key(), 0x79);
    }

    #[test]
    fn parsing_is_case_insensitive_and_canonical() {
        let hotkey = parse(" shift + control + f10 ").unwrap();
        assert_eq!(hotkey.to_string(), "Ctrl+Shift+F10");
        assert_eq!(parse("alt+ctrl+s").unwrap().to_string(), "Ctrl+Alt+S");
        assert_eq!(parse("ctrl+win+7").unwrap().to_string(), "Ctrl+Win+7");
    }

    #[test]
    fn function_keys_alone_are_allowed_except_f12() {
        assert!(parse("F9").is_ok());
        assert!(parse("F24").is_ok());
        assert_eq!(
            parse("F12"),
            Err(HotkeyError::Reserved("Windows (debugger key)"))
        );
        assert!(parse("Shift+F12").is_ok());
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(parse(""), Err(HotkeyError::MissingKey));
        assert_eq!(parse("Ctrl+Shift"), Err(HotkeyError::MissingKey));
        assert_eq!(parse("Ctrl+A+B"), Err(HotkeyError::TooManyKeys));
        assert_eq!(
            parse("Ctrl+Ctrl+A"),
            Err(HotkeyError::DuplicateModifier("Ctrl"))
        );
        assert!(matches!(
            parse("Ctrl+Space"),
            Err(HotkeyError::UnknownKey(_))
        ));
        assert!(matches!(parse("F25"), Err(HotkeyError::UnknownKey(_))));
        assert!(matches!(parse("F0"), Err(HotkeyError::UnknownKey(_))));
        assert!(matches!(parse("F01"), Err(HotkeyError::UnknownKey(_))));
    }

    #[test]
    fn typing_keys_need_a_real_modifier() {
        assert_eq!(parse("A"), Err(HotkeyError::NeedsModifier));
        assert_eq!(parse("Shift+5"), Err(HotkeyError::NeedsModifier));
        assert!(parse("Alt+5").is_ok());
    }

    #[test]
    fn reserved_shortcuts_are_rejected() {
        assert_eq!(parse("Win+G"), Err(HotkeyError::Reserved("Windows")));
        assert_eq!(parse("Win+Alt+R"), Err(HotkeyError::Reserved("Windows")));
        assert_eq!(
            parse("Alt+F4"),
            Err(HotkeyError::Reserved("Windows (close window)"))
        );
        assert_eq!(parse("Ctrl+K"), Err(HotkeyError::Reserved("Clipper")));
        assert_eq!(parse("Ctrl+2"), Err(HotkeyError::Reserved("Clipper")));
        assert!(parse("Ctrl+Shift+K").is_ok());
        assert!(parse("Ctrl+Win+F10").is_ok());
    }

    #[test]
    fn virtual_keys_match_windows_codes() {
        assert_eq!(Key::Letter('A').virtual_key(), 0x41);
        assert_eq!(Key::Digit(0).virtual_key(), 0x30);
        assert_eq!(Key::Function(1).virtual_key(), 0x70);
        assert_eq!(Key::Function(24).virtual_key(), 0x87);
    }
}
