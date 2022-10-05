use crate::Error;
use std::fmt;
use std::str::FromStr;

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct Token([u8; 32]);

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl AsRef<[u8]> for Token {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Token {
    #[inline]
    pub fn create() -> Result<Self, Error> {
        let mut buf = [0; 32];
        openssl::rand::rand_bytes(&mut buf)?;
        Ok(Self(buf))
    }
    #[inline]
    pub fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl FromStr for Token {
    type Err = Error;
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(hex::decode(s)?.as_slice().try_into()?))
    }
}
