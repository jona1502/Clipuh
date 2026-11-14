//! Validation of the custom accent field.
//!
//! The field only accepts `#` and hex digits (typed or pasted) and at most
//! [`MAX_INPUT_CHARS`] characters. A colour is applied on Enter, and only
//! when complete; anything else keeps the last valid accent and explains why.

use clipper_storage::config::{format_hex_color, parse_hex_color};

/// `#RRGGBB`.
pub(crate) const MAX_INPUT_CHARS: usize = 7;

/// Characters the accent field accepts at all.
pub(crate) fn is_hex_input_char(c: char) -> bool {
    c == '#' || c.is_ascii_hexdigit()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HexError {
    Empty,
    /// Fewer than six digits so far.
    Incomplete,
    TooLong,
    /// A character other than hex digits (or a `#` not at the start).
    Invalid,
}

impl HexError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a color like #9B5CF6.",
            Self::Incomplete => "Use six hex digits, like #9B5CF6.",
            Self::TooLong => "Use exactly six hex digits, like #9B5CF6.",
            Self::Invalid => "Only 0-9 and A-F are allowed, like #9B5CF6.",
        }
    }
}

/// Canonical `#RRGGBB` (uppercase) for `RRGGBB` or `#RRGGBB` in any case.
pub(crate) fn normalize(text: &str) -> Result<String, HexError> {
    let text = text.trim();
    if text.is_empty() || text == "#" {
        return Err(HexError::Empty);
    }
    let digits = text.strip_prefix('#').unwrap_or(text);
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(HexError::Invalid);
    }
    match digits.len() {
        0..=5 => Err(HexError::Incomplete),
        6 => parse_hex_color(&format!("#{digits}"))
            .map(format_hex_color)
            .ok_or(HexError::Invalid),
        _ => Err(HexError::TooLong),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_six_digits_in_any_case_with_or_without_hash() {
        for input in ["#9B5CF6", "#9b5cf6", "9B5CF6", "9b5Cf6", "  #9b5cf6\n"] {
            assert_eq!(normalize(input).as_deref(), Ok("#9B5CF6"), "{input:?}");
        }
        assert_eq!(normalize("#ffffff").as_deref(), Ok("#FFFFFF"));
        assert_eq!(normalize("000000").as_deref(), Ok("#000000"));
    }

    #[test]
    fn rejects_empty_partial_long_and_invalid_input() {
        assert_eq!(normalize(""), Err(HexError::Empty));
        assert_eq!(normalize("#"), Err(HexError::Empty));
        assert_eq!(normalize("#9B5"), Err(HexError::Incomplete));
        assert_eq!(normalize("9B5CF"), Err(HexError::Incomplete));
        assert_eq!(normalize("#9B5CF6FF"), Err(HexError::TooLong));
        assert_eq!(normalize("#GGGGGG"), Err(HexError::Invalid));
        assert_eq!(normalize("##9B5CF6"), Err(HexError::Invalid));
        assert_eq!(normalize("#9B 5CF6"), Err(HexError::Invalid));
        assert_eq!(normalize("rgb(1,2,3)"), Err(HexError::Invalid));
    }

    #[test]
    fn field_filter_and_length_fit_the_canonical_form() {
        assert!("#9B5CF6".chars().all(is_hex_input_char));
        assert_eq!("#9B5CF6".len(), MAX_INPUT_CHARS);
        for c in ['g', 'Z', ' ', '-', '(', ',', 'ü'] {
            assert!(!is_hex_input_char(c), "{c:?}");
        }
        for error in [
            HexError::Empty,
            HexError::Incomplete,
            HexError::TooLong,
            HexError::Invalid,
        ] {
            assert!(error.message().contains("#9B5CF6"));
        }
    }
}
