use log::trace;
use std::collections::BTreeMap;

use crate::Error;

pub struct Passwords {
    password_file: Option<String>,
    passwords: Option<BTreeMap<String, String>>,
}

impl Default for Passwords {
    fn default() -> Self {
        Self {
            password_file: None,
            passwords: None,
        }
    }
}

impl Passwords {
    pub fn set_password_file(&mut self, path: &str) {
        self.password_file.replace(path.to_owned());
    }
    /// # Errors
    ///
    /// Will return err if the file is unable to be read
    pub async fn reload(&mut self) -> Result<(), Error> {
        if let Some(path) = self.password_file.as_ref() {
            log::info!("loading password file {}", path);
            let data = tokio::fs::read_to_string(path).await?;
            let mut map = BTreeMap::new();
            for line in data.trim().lines() {
                let parts: Vec<&str> = line.trim().split(':').collect();
                if parts.len() != 2 {
                    continue;
                }
                if parts[0].starts_with('#') {
                    continue;
                }
                let login = parts[0].trim().to_owned();
                trace!("loaded password for \"{}\"", login);
                map.insert(login, parts[1].trim().to_owned());
            }
            self.passwords.replace(map);
        }
        Ok(())
    }
    pub fn verify(&self, login: &str, password: &str) -> bool {
        if let Some(map) = self.passwords.as_ref() {
            if let Some(hash) = map.get(login) {
                return bcrypt::verify(password, hash).unwrap_or(false);
            }
        }
        false
    }
}
