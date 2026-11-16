//! The operating system's random generator: the adapter of the entropy port.

use ports::Entropy;

/// Draws random bits from the thread-local generator seeded by the operating system.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn random_u128(&self) -> u128 {
        rand::random::<u128>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_draws_differ() {
        assert_ne!(OsEntropy.random_u128(), OsEntropy.random_u128());
    }
}
