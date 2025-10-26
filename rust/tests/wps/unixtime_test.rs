use crate::common::{ZenOutput, ZenTest};

impl ZenTest {
    pub fn wps_unixtime(&self, args: &[&str]) -> ZenOutput {
        let mut cmd = self.zen_command();
        cmd.args(["wps", "unixtime"]);
        cmd.args(args);

        let output = cmd.output().expect("Failed to execute zen command");
        ZenOutput::new(self.temp_dir.path().to_str().unwrap().into(), output)
    }
}

#[test]
fn test_wps_unixtime_current() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&[]);
    assert!(output.success());
    assert!(output.stdout().contains("timestamp:"));
}

#[test]
fn test_wps_unixtime_default_timeunit() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["1761473983"]);
    assert!(output.success());
    assert!(output.stdout().contains("timestamp: 1761473983"));
}

#[test]
fn test_wps_unixtime_second_timeunit() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["-t", "s", "1761473983"]);
    assert!(output.success());
    assert!(output.stdout().contains("timestamp: 1761473983"));
}

#[test]
fn test_wps_unixtime_millis_timeunit() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["-t", "ms", "1761473529558"]);
    println!("{}", output.stdout());
    assert!(output.success());
    assert!(output.stdout().contains("timestamp millis: 1761473529558"));
}

#[test]
fn test_wps_unixtime_micros_timeunit() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["-t", "us", "1761473529558076"]);
    println!("{}", output.stdout());
    assert!(output.success());
    assert!(
        output
            .stdout()
            .contains("timestamp micros: 1761473529558076")
    );
}

#[test]
fn test_wps_unixtime_nanos_timeunit() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["-t", "ns", "1761473529558076010"]);
    println!("{}", output.stdout());
    assert!(output.success());
    assert!(
        output
            .stdout()
            .contains("timestamp nanos: 1761473529558076010")
    );
}

#[test]
fn test_wps_unixtime_timeunit_error() {
    let test = ZenTest::new();
    let output = test.wps_unixtime(&["-t", "ms", "1761473529558076010"]);
    println!("{}", output.stderr());
    assert!(!output.success());
    assert!(output.stderr().contains("No such local time"));
}
