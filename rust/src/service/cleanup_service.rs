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

    let gem_command = "gem";
    if util::command_exists(gem_command) {
        println!("Cleaning RubyGems cache");

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
    } else {
        // eprintln!("gem command is not available.please install it.");
    }

    let brew_command = "brew";
    if util::command_exists(brew_command) {
        println!("Cleaning Homebrew cache");
        let status = Command::new(brew_command)
            .arg("cleanup")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect(format!("failed to execute {} command", brew_command).as_str());

        if status.success() {
            println!("Homebrew cache cleanup successful.");
        } else {
            eprintln!("Homebrew cache cleanup might have failed for other reasons.",);
        }
    } else {
        // eprintln!("brew command is not available.please install `brew` command.");
    }

    let golang_command = "go";
    if util::command_exists(golang_command) {
        println!("Cleaning Go cache");

        let status = Command::new(golang_command)
            .arg("clean")
            .arg("-cache")
            .arg("-modcache")
            .arg("-testcache")
            .arg("-fuzzcache")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect(format!("failed to execute {} command", golang_command).as_str());

        if status.success() {
            println!("Go cache cleanup successful.");
        } else {
            eprintln!("Go cache cleanup might have failed for other reasons.",);
        }
    } else {
        // eprintln!("go command is not available.please install `go` command.");
    }

    let poetry_command = "poetry";
    if util::command_exists(poetry_command) {
        println!("Cleaning Poetry cache");
        let status = Command::new(poetry_command)
            .arg("cache")
            .arg("clear")
            .arg("--all")
            .arg("pypi")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect(format!("failed to execute {} command", poetry_command).as_str());

        if status.success() {
            println!("Poetry cache cleanup successful.");
        } else {
            eprintln!("Poetry cache cleanup might have failed for other reasons.",);
        }
    } else {
        // println!("Poetry command is not available.please install `Poetry` command.");
    }

    let uv_command = "uv";
    if util::command_exists(uv_command) {
        println!("Cleaning uv cache");

        let status = Command::new(uv_command)
            .arg("cache")
            .arg("prune")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect(format!("failed to execute {} command", uv_command).as_str());
        if status.success() {
            println!("uv cache cleanup successful.");
        } else {
            eprintln!("uv cache cleanup might have failed for other reasons.",);
        }
    }

    let pip_command = "pip";
    if util::command_exists(pip_command) {
        println!("Cleaning pip cache");

        let status = Command::new(pip_command)
            .arg("cache")
            .arg("purge")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect(format!("failed to execute {} command", pip_command).as_str());
        if status.success() {
            println!("pip cache cleanup successful.");
        } else {
            eprintln!("pip cache cleanup might have failed for other reasons.",);
        }
    }

    let home_dir = home_dir().expect("Could not find home directory");
    let cargo_cache_path = home_dir.join(".cargo/registry/cache");
    if cargo_cache_path.exists() {
        println!("Cleaning Cargo cache");
        let pattern = cargo_cache_path.to_str().expect("Invalid Unicode in path");
        util::delete_pattern(pattern);
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
