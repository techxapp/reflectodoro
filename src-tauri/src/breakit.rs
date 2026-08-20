use rand::Rng;

const ALPHANUMERIC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const SPECIAL: &[u8] = b"!@#$%^&*()-_=+[]{}";

/// Fresh random string generated per overlay -- typing it out (no paste
/// allowed) is the early-exit alternative to waiting out the break timer.
/// Being random rather than a fixed word means it can't become muscle memory.
pub fn generate_challenge(length: u32, include_special: bool) -> String {
    let mut charset: Vec<u8> = ALPHANUMERIC.to_vec();
    if include_special {
        charset.extend_from_slice(SPECIAL);
    }

    let mut rng = rand::thread_rng();
    (0..length.clamp(4, 64))
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_length() {
        assert_eq!(generate_challenge(15, false).chars().count(), 15);
        assert_eq!(generate_challenge(2, false).chars().count(), 4); // clamped up
        assert_eq!(generate_challenge(200, false).chars().count(), 64); // clamped down
    }

    #[test]
    fn excludes_special_by_default() {
        let s = generate_challenge(200_u32.min(64), false);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn can_include_special() {
        // Generate many long samples; at least one special char should show up.
        let found = (0..20).any(|_| {
            generate_challenge(64, true)
                .chars()
                .any(|c| !c.is_ascii_alphanumeric())
        });
        assert!(found);
    }
}
