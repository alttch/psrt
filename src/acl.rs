use crate::Error;
use log::{info, trace};
use once_cell::sync::Lazy;
use serde::{ser::SerializeSeq, Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

static ERR_PATH_MASK_EMPTY: &str = "Empty path mask";

pub static ACL_DB: Lazy<RwLock<Db>> = Lazy::new(<_>::default);

#[derive(Debug, Default)]
pub struct Db {
    acls: BTreeMap<String, Arc<Acl>>,
    path: PathBuf,
}

impl Db {
    #[inline]
    pub fn get_acl(&self, user: &str) -> Option<Arc<Acl>> {
        self.acls.get(user).cloned()
    }
    #[inline]
    pub fn has_acl(&self, user: &str) -> bool {
        self.acls.contains_key(user)
    }
    #[inline]
    pub fn set_path(&mut self, path: &Path) {
        path.clone_into(&mut self.path);
    }
    /// # Errors
    ///
    /// Will return err on file read / deserialize error
    pub async fn reload(&mut self) -> Result<(), Error> {
        info!("loading ACL {}", self.path.to_string_lossy());
        let acls: BTreeMap<String, Acl> =
            serde_yaml::from_str(&tokio::fs::read_to_string(&self.path).await?)?;
        self.acls.clear();
        // TODO optimize when pop available
        for (user, acl) in acls {
            self.acls.insert(user, Arc::new(acl));
        }
        trace!("{:?}", self.acls);
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acl {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    replicator: bool,
    #[serde(default, rename = "sub")]
    read: Option<PathMaskList>,
    #[serde(default, rename = "pub")]
    write: Option<PathMaskList>,
}

impl Acl {
    pub fn new_full() -> Self {
        let mut read = HashSet::new();
        let mut write = HashSet::new();
        read.insert(PathMask::new_any());
        write.insert(PathMask::new_any());
        Self {
            admin: true,
            replicator: true,
            read: Some(PathMaskList::new(read)),
            write: Some(PathMaskList::new(write)),
        }
    }
    #[inline]
    pub fn allow_read(&self, topic: &str) -> bool {
        self.read.as_ref().map_or(false, |v| v.matches(topic))
    }
    #[inline]
    pub fn allow_write(&self, topic: &str) -> bool {
        self.write.as_ref().map_or(false, |v| v.matches(topic))
    }
    #[inline]
    pub fn is_replicator(&self) -> bool {
        self.replicator
    }
    #[inline]
    pub fn is_admin(&self) -> bool {
        self.admin
    }
}

#[derive(Debug, Clone, Eq)]
pub struct PathMask {
    chunks: Option<Vec<String>>,
}

impl PathMask {
    fn new_any() -> Self {
        Self { chunks: None }
    }
    #[inline]
    fn is_str_any(s: &str) -> bool {
        s == "#"
    }
    #[inline]
    fn is_str_wildcard(s: &str) -> bool {
        s == "+"
    }
    fn matches_split(&self, path_split: &mut std::str::Split<'_, char>) -> bool {
        if let Some(ref chunks) = self.chunks {
            let mut s_m = chunks.iter();
            loop {
                if let Some(i_chunk) = path_split.next() {
                    if let Some(m_chunk) = s_m.next() {
                        if PathMask::is_str_any(m_chunk) {
                            return true;
                        }
                        if !PathMask::is_str_wildcard(m_chunk) && i_chunk != m_chunk {
                            return false;
                        }
                    } else {
                        return false;
                    }
                } else {
                    return s_m.next().is_none();
                }
            }
        } else {
            true
        }
    }
}

impl<'de> Deserialize<'de> for PathMask {
    fn deserialize<D>(deserializer: D) -> Result<PathMask, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_unit(PathMaskVisitor)
    }
}

struct PathMaskVisitor;
impl<'de> serde::de::Visitor<'de> for PathMaskVisitor {
    type Value = PathMask;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string-packed path mask")
    }
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        value
            .parse()
            .map_err(|e| E::custom(format!("{}: {}", e, value)))
    }
    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        value
            .parse()
            .map_err(|e| E::custom(format!("{}: {}", e, value)))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct PathMaskList {
    pub path_masks: HashSet<PathMask>,
}

impl Serialize for PathMaskList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.path_masks.len()))?;
        for element in &self.path_masks {
            seq.serialize_element(&element.to_string())?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for PathMaskList {
    fn deserialize<D>(deserializer: D) -> Result<PathMaskList, D::Error>
    where
        D: Deserializer<'de>,
    {
        let masks: HashSet<PathMask> = Deserialize::deserialize(deserializer)?;
        Ok(PathMaskList::new(masks))
    }
}

impl PathMaskList {
    pub fn new(path_masks: HashSet<PathMask>) -> Self {
        Self { path_masks }
    }

    pub fn matches(&self, path: &str) -> bool {
        self.matches_split(path.split('/'))
    }

    fn matches_split(&self, path_split: std::str::Split<'_, char>) -> bool {
        for path_mask in &self.path_masks {
            if path_mask.matches_split(&mut path_split.clone()) {
                return true;
            }
        }
        false
    }
    pub fn is_empty(&self) -> bool {
        self.path_masks.is_empty()
    }
    /// # Errors
    ///
    /// Will return Err if any mask is unable to be parsed
    pub fn from_str_list(s_masks: &[&str]) -> Result<Self, Error> {
        let mut path_masks = HashSet::new();
        for s in s_masks {
            path_masks.insert(s.parse()?);
        }
        Ok(Self { path_masks })
    }

    /// # Errors
    ///
    /// Will return Err if any mask is unable to be parsed
    pub fn from_string_vec(s_masks: &[String]) -> Result<Self, Error> {
        let mut path_masks = HashSet::new();
        for s in s_masks {
            path_masks.insert(s.parse()?);
        }
        Ok(Self { path_masks })
    }
}

impl PartialEq for PathMask {
    fn eq(&self, other: &Self) -> bool {
        self.chunks == other.chunks
    }
}

impl Ord for PathMask {
    fn cmp(&self, other: &Self) -> Ordering {
        self.chunks.cmp(&other.chunks)
    }
}

impl Hash for PathMask {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.chunks.hash(hasher);
    }
}

impl PartialOrd for PathMask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PathMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref chunks) = self.chunks {
            write!(f, "{}", chunks.join("/"))
        } else {
            write!(f, "#")
        }
    }
}

impl FromStr for PathMask {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            Err(Error::invalid_data(ERR_PATH_MASK_EMPTY))
        } else if PathMask::is_str_any(s) {
            Ok(Self::new_any())
        } else {
            let mut chunks = Vec::new();
            for chunk in s.split('/') {
                if PathMask::is_str_any(chunk) {
                    chunks.push("#".to_owned());
                    break;
                }
                chunks.push(chunk.to_owned());
            }
            Ok(Self {
                chunks: Some(chunks),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PathMask, PathMaskList};

    #[test]
    fn test_path_mask() {
        let s = "#";
        let mask: PathMask = s.parse().unwrap();
        assert_eq!(s, mask.to_string());
        assert_eq!(mask.chunks, None);
        let s = "";
        assert!(s.parse::<PathMask>().is_err());
        let s = "data/#";
        let mask: PathMask = s.parse().unwrap();
        assert_eq!(s, mask.to_string());
        assert_eq!(mask.chunks.unwrap(), ["data", "#"]);
        let s = "data/tests/t1";
        let mask: PathMask = s.parse().unwrap();
        assert_eq!(s, mask.to_string());
        assert_eq!(mask.chunks.unwrap(), ["data", "tests", "t1"]);
        let s = "data/tests/#";
        let mask: PathMask = s.parse().unwrap();
        assert_eq!(mask.to_string(), "data/tests/#");
        assert_eq!(mask.chunks.unwrap(), ["data", "tests", "#"]);
        let s = "data/#/t1";
        let mask: PathMask = s.parse().unwrap();
        assert_ne!(s, mask.to_string());
        assert_eq!(mask.chunks.unwrap(), ["data", "#"]);
    }

    #[test]
    fn test_path_mask_list() {
        let p =
            PathMaskList::from_str_list(&["test/tests", "+/xxx", "zzz/+/222", "abc", "a/b/#/c"])
                .unwrap();
        assert!(!p.matches("test"));
        assert!(p.matches("test/tests"));
        assert!(!p.matches("test/tests2"));
        assert!(p.matches("aaa/xxx"));
        assert!(!p.matches("aaa/xxx/123"));
        assert!(p.matches("zzz/xxx/222"));
        assert!(!p.matches("zzz/xxx/222/555"));
        assert!(!p.matches("zzz/xxx/223"));
        assert!(p.matches("abc"));
        assert!(!p.matches("abd"));
        assert!(p.matches("abc/xxx"));
        assert!(!p.matches("abc/zzz"));
        assert!(p.matches("a/b/zzz"));
        assert!(p.matches("a/b/zzz/xxx"));
        let p = PathMaskList::from_str_list(&["#"]).unwrap();
        assert!(p.matches("test"));
        assert!(p.matches("test/tests"));
        assert!(p.matches("test/tests2"));
        assert!(p.matches("aaa/xxx"));
        assert!(p.matches("aaa/xxx/123"));
        assert!(p.matches("zzz/xxx/222"));
        assert!(p.matches("zzz/xxx/222/555"));
        assert!(p.matches("zzz/xxx/223"));
        assert!(p.matches("abc"));
        assert!(p.matches("abd"));
        assert!(p.matches("abc/xxx"));
        assert!(p.matches("abc/zzz"));
        assert!(p.matches("a/b/zzz"));
        assert!(p.matches("a/b/zzz/xxx"));
    }
}
