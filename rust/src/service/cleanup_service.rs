use fs_extra;
use glob::glob;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

use home::home_dir;

pub(crate) fn clean_all() {
    clean_trash();

    clean_cache();

    clean_logs();
}

pub(crate) fn clean_trash() {
    println!("Clean Trash ...");

    let home = home_dir().unwrap();
    let trash_path = home.join(".Trash/*");

    // Collect all matching paths.
    let mut delete_list: Vec<PathBuf> = Vec::new();
    let pattern = trash_path.to_str().unwrap();
    for entry in glob(pattern).unwrap() {
        if let Ok(path) = entry {
            delete_list.push(path);
        }
    }

    println!("path:{}", trash_path.display());
    println!("path:{:?}", delete_list);

    let osascript_command = r#"
           try
               tell application "Finder" to empty trash
           on error number -128
               -- 垃圾桶已空，忽略此錯誤
           end try
       "#;

    let status = Command::new("osascript")
        .arg("-e")
        .arg(osascript_command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("failed to execute osascript command");

    if status.success() {
        println!("Trash cleanup successful.");
    } else {
        eprintln!("Trash cleanup might have failed for other reasons.");
    }
}
pub(crate) fn clean_cache() {
    println!("Clean Cache ...");
}

pub(crate) fn clean_logs() {
    println!("Clean Logs ...");

    println!("Deleting Java heap dumps");
    let home_dir = home_dir().expect("Could not find home directory");

    // Create the full glob pattern, expanding the tilde (~) to the home directory.
    let heap_path = home_dir.join("*.hprof");
    let pattern = heap_path.to_str().expect("Invalid Unicode in path");

    // Collect all matching paths.
    let mut delete_list: Vec<PathBuf> = Vec::new();
    for entry in glob(pattern).unwrap() {
        if let Ok(path) = entry {
            delete_list.push(path);
        }
    }
    // Use fs_extra to remove the collected items.
    if !delete_list.is_empty() {
        println!("Deleting the following files: {:?}", delete_list);
        fs_extra::remove_items(&delete_list).expect("Deleting *.hprof files failed");
    } else {
        println!("No *.hprof files found to delete.");
    }

    println!("Clearing all application log files from JetBrains:");
    let path = "Library/Logs/JetBrains/*/";
    let jet_logs_path = home_dir.join(path);

    let pattern = jet_logs_path.to_str().expect("Invalid Unicode in path");

    // Collect all matching paths.
    let mut delete_list: Vec<PathBuf> = Vec::new();
    for entry in glob(pattern).unwrap() {
        if let Ok(path) = entry {
            delete_list.push(path);
        }
    }
    // Use fs_extra to remove the collected items.
    if !delete_list.is_empty() {
        println!("Deleting the following files: {:?}", delete_list);
        fs_extra::remove_items(&delete_list)
            .expect("Deleting Library/Logs/JetBrains/*/ files failed");
    } else {
        println!("No Library/Logs/JetBrains/*/ files found to delete.");
    }
}
