use assert_cmd::Command;

#[test]
fn oratorsctl_help_works() {
    Command::cargo_bin("oratorsctl")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn orators_help_works() {
    Command::cargo_bin("orators")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn oratorsd_help_works() {
    Command::cargo_bin("oratorsd")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}
