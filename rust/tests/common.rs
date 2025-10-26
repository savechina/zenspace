// use mockito::Mock;
use std::process::Command;
use std::{collections::HashMap, path::PathBuf};
use tempfile::TempDir;

pub struct ZenTest {
    pub temp_dir: TempDir,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    // For mocking the releases json from Github API
    // pub server: mockito::ServerGuard,
}

impl ZenTest {
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temporary directory");
        let cwd = temp_dir.path().into();

        let mut test = Self {
            temp_dir,
            cwd,
            env: HashMap::new(),
            // server: mockito::Server::new(),
        };

        test.env.insert(
            "ZEN_ROOT_DIR".into(),
            test.temp_dir.path().to_str().unwrap().into(),
        );
        // Set consistent arch/os for cross-platform testing
        test.env
            .insert("ZEN_TEST_PLATFORM".into(), "aarch64-apple-darwin".into()); // For mocking current_platform::CURRENT_PLATFORM
        test.env.insert("ZEN_TEST_ARCH".into(), "aarch64".into());
        test.env.insert("ZEN_TEST_OS".into(), "macos".into());

        test.env
            .insert("ZEN_TEST_EXE".into(), "/tmp/bin/zen".into());
        test.env.insert("HOME".into(), "/tmp/home".into());
        test.env.insert("ZEN_DISABLE_INDICATIF".into(), "1".into()); // Disable indicatif progress bars in tests due to a bug in tracing-indicatif

        // Disable network requests by default
        // test.env.insert("ZEN_RELEASES_URL".into(), test.server.url());

        // Disable caching for tests by default
        test.env.insert("ZEN_NO_CACHE".into(), "true".into());

        test
    }

    pub fn zen(&self, args: &[&str]) -> ZenOutput {
        let mut cmd = self.zen_command();
        cmd.args(args);

        let output = cmd.output().expect("Failed to execute zen command");
        ZenOutput::new(self.temp_dir.path().display().to_string().as_str(), output)
    }

    pub fn zen_command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zen"));
        cmd.current_dir(&self.cwd);
        cmd.env_clear().envs(&self.env);
        cmd
    }
}

pub struct ZenOutput {
    pub output: std::process::Output,
    pub test_root: String,
}

impl ZenOutput {
    pub fn new(test_root: &str, output: std::process::Output) -> Self {
        Self {
            output,
            test_root: test_root.into(),
        }
    }

    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    #[track_caller]
    pub fn assert_success(&self) -> &Self {
        assert!(
            self.success(),
            "Expected command to succeed, got:\n\n# STDERR\n{}\n# STDOUT\n{}\n# STATUS {:?}",
            std::str::from_utf8(&self.output.stderr).unwrap(),
            std::str::from_utf8(&self.output.stdout).unwrap(),
            self.output.status
        );
        self
    }

    #[track_caller]
    pub fn assert_failure(&self) -> &Self {
        assert!(
            !self.success(),
            "Expected command to fail, got:\n\n# STDERR\n{}\n# STDOUT\n{}",
            std::str::from_utf8(&self.output.stderr).unwrap(),
            std::str::from_utf8(&self.output.stdout).unwrap(),
        );
        self
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).to_string()
    }

    #[allow(dead_code)]
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).to_string()
    }
}
