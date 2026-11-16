//! Randomness as a port: the core asks for random bits only through this trait, so what a run
//! records does not depend on a generator hidden in the library.

/// A source of random bits.
pub trait Entropy {
    /// 128 fresh random bits.
    fn random_u128(&self) -> u128;
}
