use std::path::PathBuf;

use which;

use include_dir::{Dir, include_dir};

/// Assets
static ASSETS: Dir = include_dir!("assets");

/// Template
static TEMPLATES: Dir = include_dir!("templates");

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
