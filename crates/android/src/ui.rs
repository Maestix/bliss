//! Words, for the few lines the engine writes itself: toasts and errors.
//! Everything else the screens say is in the Kotlin side's resources.

/// Russian plural agreement, as the desktop's `plural` in ui.rs: one, few,
/// many by the last two digits.
pub fn plural(n: u64, one: &str, few: &str, many: &str) -> String {
    let word = match (n % 10, n % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    };
    format!("{n} {word}")
}

#[cfg(test)]
mod tests {
    use super::plural;

    #[test]
    fn russian_numbers_agree() {
        let s = |n| plural(n, "сервер", "сервера", "серверов");
        assert_eq!(s(1), "1 сервер");
        assert_eq!(s(3), "3 сервера");
        assert_eq!(s(5), "5 серверов");
        assert_eq!(s(11), "11 серверов");
        assert_eq!(s(21), "21 сервер");
        assert_eq!(s(112), "112 серверов");
    }
}
