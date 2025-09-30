use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use home::home_dir;

use crate::util;

/// clean all logs and cache file
pub(crate) fn clean_all() {
    //clean system trash
    clean_trash();

    //clean application cache
    clean_cache();

    //clean application logs
    clean_logs();

    //clean ide project config file
    clean_ide();
}

pub(crate) fn clean_trash() {
    println!("Clean Trash ...");

    let home = home_dir().unwrap();
    let trash_path = home.join(".Trash/*");

    println!("path:{}", trash_path.display());

    let osascript_command = r#"
           try
               tell application "Finder" to empty trash
           on error number -128
               -- 垃圾桶已空，忽略此錯誤
           end try
       "#;

    if !util::command_exists("osascript") {
        println!("osascript command is not available.please check your macos version.");
    }

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

    println!("Cleaning RubyGems cache");

    let gem_command = "gem";
    if !util::command_exists(gem_command) {
        println!("osascript command is not available.please check your macos version.");
    }

    let status = Command::new(gem_command)
        .arg("cleanup")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect(format!("failed to execute {} command", gem_command).as_str());

    if status.success() {
        println!("{} cache cleanup successful.", gem_command);
    } else {
        eprintln!(
            "{} cache cleanup might have failed for other reasons.",
            gem_command
        );
    }

    println!("Cleaning Homebrew cache");

    let brew = "brew";
    if !util::command_exists(brew) {
        println!("brew command is not available.please install `brew` command.");
    }

    let status = Command::new(brew)
        .arg("cleanup")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect(format!("failed to execute {} command", brew).as_str());

    if status.success() {
        println!("Homebrew cache cleanup successful.");
    } else {
        eprintln!("Homebrew cache cleanup might have failed for other reasons.",);
    }
}

pub(crate) fn clean_logs() {
    println!("Clean Logs ...");
    let home_dir = home_dir().expect("Could not find home directory");

    println!("Deleting Java heap dumps");
    let heap_path = home_dir.join("*.hprof");
    let pattern = heap_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from JetBrains:");
    let jet_logs_path = home_dir.join("Library/Logs/JetBrains/*/");
    let pattern = jet_logs_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from Notion:");
    let notion_logs_path = home_dir.join("Library/Logs/Notion/*");
    let pattern = notion_logs_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from Zed:");
    let zen_logs_path = home_dir.join("Library/Logs/Zed/*");
    let pattern = zen_logs_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from Arduino IDE:");
    let arduino_logs_path = home_dir.join("Library/Logs/Arduino IDE/*");
    let pattern = arduino_logs_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from DiagnosticReports:");
    let diagnostic_logs_path = home_dir.join("Library/Logs/DiagnosticReports/*");
    let pattern = diagnostic_logs_path
        .to_str()
        .expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from iPhone Updater Logs:");
    let iphone_updater_logs_path = home_dir.join("Library/Logs/iPhone Updater Logs/*");
    let pattern = iphone_updater_logs_path
        .to_str()
        .expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Clearing all application log files from /var/logs/*.log Logs:");
    let var_logs_path = PathBuf::from("/var/logs/*.log*");
    let pattern = var_logs_path.to_str().expect("Invalid Unicode in path");
    util::delete_pattern(pattern);

    println!("Logs cleanup successful.");
}

pub(crate) fn clean_ide() {
    println!("Clean IDE project file ...");

    println!("IDE project file cleanup successful.");
}
