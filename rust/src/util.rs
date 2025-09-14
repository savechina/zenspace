use anyhow::Result;
use std::{env, path::PathBuf, sync::LazyLock};
use which;

use include_dir::{Dir, include_dir};

/// Assets
pub(crate) static ASSETS: Dir = include_dir!("assets");

/// Template
pub(crate) static TEMPLATES: Dir = include_dir!("templates");

///User Root Directory
pub(crate) static USER_ROOT: LazyLock<PathBuf> = LazyLock::new(|| -> PathBuf {
    home::home_dir()
        .map(|home| PathBuf::from(home).join(".zen"))
        .unwrap()
});

/// which `command` exists
pub(crate) fn command_exists(command: &str) -> bool {
    let result = which::which(command);

    let exists = match result {
        Ok(path) => {
            println!("path:{:?}", path);
            true
        }
        Err(_) => false,
    };

    exists
}
