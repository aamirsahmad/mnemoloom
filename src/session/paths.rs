//! The one cache root every `mnemoloom` cache lives under.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::homes::{expand_tilde, user_home};

/// Env override for the whole cache root.
pub const CACHE_DIR_ENV: &str = "MNEMOLOOM_CACHE_DIR";

/// `$MNEMOLOOM_CACHE_DIR` (`~` expanded) > `$XDG_CACHE_HOME/mnemoloom` (when
/// absolute) > `~/.cache/mnemoloom`, on every platform.
pub fn cache_root() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    cache_root_from(env(CACHE_DIR_ENV), env("XDG_CACHE_HOME"), &user_home())
}

fn cache_root_from(mnemoloom: Option<OsString>, xdg: Option<OsString>, home: &Path) -> PathBuf {
    if let Some(d) = mnemoloom {
        return expand_tilde(Path::new(&d));
    }
    xdg.map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".cache"))
        .join("mnemoloom")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(mnemoloom: Option<&str>, xdg: Option<&str>) -> PathBuf {
        cache_root_from(
            mnemoloom.map(Into::into),
            xdg.map(Into::into),
            Path::new("/h"),
        )
    }

    #[test]
    fn precedence() {
        assert_eq!(root(Some("/m"), Some("/x")), PathBuf::from("/m"));
        assert_eq!(root(Some("~/m"), None), user_home().join("m"));
        assert_eq!(root(None, Some("/x")), PathBuf::from("/x/mnemoloom"));
        assert_eq!(
            root(None, Some("rel")),
            PathBuf::from("/h/.cache/mnemoloom")
        );
        assert_eq!(root(None, None), PathBuf::from("/h/.cache/mnemoloom"));
    }
}
