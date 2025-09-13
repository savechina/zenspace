use std::path::PathBuf;

use which;

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
