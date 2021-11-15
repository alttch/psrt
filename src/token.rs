use std::fmt;
use rand::Rng;

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct Token([u8; 32]);

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl Token {
    /// # Panics
    ///
    /// Should not panic
    pub fn new() -> Self {
        Self(
            rand::thread_rng()
                .sample_iter(&rand::distributions::Uniform::new(0, 0xff))
                .take(32)
                .map(u8::from)
                .collect::<Vec<u8>>()
                .try_into()
                .unwrap(),
        )
    }
    pub fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Default for Token {
    fn default() -> Self {
        Self::new()
    }
}
