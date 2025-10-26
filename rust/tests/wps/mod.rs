use crate::common::ZenTest;

#[test]
fn test_shell_init_succeeds() {
    let test = ZenTest::new();
    let output = test.zen(&["wps", "unixtime"]);
    output.assert_success();
    println!("test... done: {}", output.stdout());
}
