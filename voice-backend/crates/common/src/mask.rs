//! Masking of personal data before it reaches logs.

/// Masks a phone number for logs, keeping only the last four digits:
/// `+919876543210` becomes `********3210`. Numbers with four digits or
/// fewer are fully masked.
pub fn mask_phone(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    if digits.len() <= 4 {
        return "****".to_owned();
    }
    let split = digits.len() - 4;
    let tail: String = digits[split..].iter().collect();
    format!("{}{tail}", "*".repeat(split))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_four_digits() {
        assert_eq!(mask_phone("+91 98765 43210"), "********3210");
        assert_eq!(mask_phone("1234"), "****");
        assert_eq!(mask_phone(""), "****");
    }
}
