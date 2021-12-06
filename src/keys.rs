use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, NewAead};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};

use crate::token::Token;
use crate::Error;

#[derive(Eq, PartialEq, Debug, Copy, Clone)]
#[repr(u8)]
pub enum EncryptionType {
    No = crate::AUTH_LOGIN_PASS,
    Aes128Gcm = crate::AUTH_KEY_AES128_GCM,
    Aes256Gcm = crate::AUTH_KEY_AES256_GCM,
}

impl EncryptionType {
    /// # Errors
    ///
    /// With return error if the encryption format is not supported
    #[inline]
    pub fn from_byte(b: u8) -> Result<Self, Error> {
        match b {
            crate::AUTH_LOGIN_PASS => Ok(EncryptionType::No),
            crate::AUTH_KEY_AES128_GCM => Ok(EncryptionType::Aes128Gcm),
            crate::AUTH_KEY_AES256_GCM => Ok(EncryptionType::Aes256Gcm),
            _ => Err(Error::invalid_data("unsupported encryption type")),
        }
    }
    #[inline]
    pub fn need_decrypt(self) -> bool {
        self != EncryptionType::No
    }
}

pub struct Key {
    cipher_aes_128: Aes128Gcm,
    cipher_aes_256: Aes256Gcm,
}

pub struct Keys {
    key_file: Option<String>,
    keys: Option<BTreeMap<String, Key>>,
    nonce: [u8; 12],
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            key_file: None,
            keys: None,
            nonce: <_>::default(),
        }
    }
}

impl Keys {
    pub fn set_nonce(&mut self, nonce: Option<[u8; 12]>) {
        if let Some(nonce) = nonce {
            self.nonce = nonce;
        } else {
            log::warn!("AES nonce not defined, using zeroes");
            self.nonce = <_>::default()
        }
    }
    pub fn set_key_file(&mut self, path: &str) {
        self.key_file.replace(path.to_owned());
    }
    /// # Errors
    ///
    /// Will return err if the file is unable to be read or parsed
    pub async fn reload(&mut self) -> Result<(), Error> {
        if let Some(path) = self.key_file.as_ref() {
            log::info!("loading key file {}", path);
            let data = tokio::fs::read_to_string(path).await?;
            let keys: BTreeMap<String, String> = serde_yaml::from_str(&data)?;
            let mut key_map = BTreeMap::new();
            let acl_db = crate::acl::ACL_DB.read().await;
            for (login, k) in keys {
                log::trace!("+ key {} ({})", k, login);
                match k.parse::<Token>() {
                    Ok(token) => {
                        if acl_db.has_acl(&login) {
                            let aes_key_128 = aes_gcm::Key::from_slice(&token.as_bytes()[0..16]);
                            let aes_key_256 = aes_gcm::Key::from_slice(token.as_bytes());
                            let key = Key {
                                cipher_aes_128: Aes128Gcm::new(aes_key_128),
                                cipher_aes_256: Aes256Gcm::new(aes_key_256),
                            };
                            key_map.insert(login, key);
                        } else {
                            log::warn!("No ACL defined for the key {}", login);
                        }
                    }
                    Err(e) => log::error!("Unable to parse key: {}", e),
                }
            }
            self.keys.replace(key_map);
        }
        Ok(())
    }
    /// # Errors
    ///
    /// With return auth errors if: the key with such id is not defined or on decryption errors
    ///
    /// # Panics
    ///
    /// Will panic if attempted to decrypt unencrypted packet
    pub fn auth_and_decr(
        &self,
        block: &[u8],
        key_id: &str,
        tp: EncryptionType,
    ) -> Result<Vec<u8>, Error> {
        if let Some(Some(key)) = self.keys.as_ref().map(|m| m.get(key_id)) {
            match tp {
                EncryptionType::Aes128Gcm => key
                    .cipher_aes_128
                    .decrypt(&Nonce::from_slice(&self.nonce), block)
                    .map_err(Into::into),
                EncryptionType::Aes256Gcm => key
                    .cipher_aes_256
                    .decrypt(&Nonce::from_slice(&self.nonce), block)
                    .map_err(Into::into),
                EncryptionType::No => panic!("Attempt to decrypt unencrypted"),
            }
        } else {
            Err(Error::access(format!("no such key id: {}", key_id)))
        }
    }
}
