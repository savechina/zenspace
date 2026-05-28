pub(crate) fn command_exists(command: &str) -> bool {
    let result = which::which(command);

    match result {
        Ok(path) => {
            println!("path:{}", path.display());
            true
        },
        Err(_) => false,
    }
}

pub(crate) fn delete_pattern(path_pattern: &str) {
    let mut file_list: Vec<std::path::PathBuf> = Vec::new();

    for entry in glob::glob(path_pattern).unwrap().flatten() {
        file_list.push(entry);
    }

    if !file_list.is_empty() {
        println!("Deleting the following files: {:?}", file_list);
        fs_extra::remove_items(&file_list)
            .unwrap_or_else(|_| panic!("Deleting {} files failed", path_pattern));
    } else {
        println!("No {} files found to delete.", path_pattern);
    }
}
