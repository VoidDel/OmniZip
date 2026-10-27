//! End-to-end tests of the `omnizip` binary: exit codes, JSON contract, non-interactive behaviour.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn omnizip(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omnizip"))
        .current_dir(dir)
        .args(args)
        .args(["--lang", "en-US", "--no-input", "--color", "never"])
        .env_remove("OMNIZIP_LANG")
        .output()
        .expect("run omnizip")
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn fixture_tree(dir: &Path) {
    fs::create_dir_all(dir.join("proj/src")).unwrap();
    fs::write(dir.join("proj/src/main.rs"), "fn main() {}\n".repeat(100)).unwrap();
    fs::write(dir.join("proj/读我.md"), "# 你好\n").unwrap();
}

#[test]
fn create_list_test_extract_round_trip() {
    let d = tempfile::tempdir().unwrap();
    fixture_tree(d.path());
    for name in ["p.zip", "p.7z", "p.tar.gz", "p.tar.zst"] {
        let o = omnizip(d.path(), &["a", name, "proj"]);
        assert_eq!(code(&o), 0, "create {name}: {}", stderr(&o));

        let o = omnizip(d.path(), &["l", name, "--json"]);
        assert_eq!(code(&o), 0);
        let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
        let paths: Vec<&str> = v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"proj/读我.md"), "{name}: {paths:?}");

        let o = omnizip(d.path(), &["t", name]);
        assert_eq!(code(&o), 0, "{}", stderr(&o));

        let out = format!("out-{name}");
        let o = omnizip(d.path(), &["x", name, "-o", &out]);
        assert_eq!(code(&o), 0, "{}", stderr(&o));
        assert_eq!(
            fs::read_to_string(d.path().join(&out).join("proj/读我.md")).unwrap(),
            "# 你好\n"
        );
    }
}

#[test]
fn exit_codes_follow_the_contract() {
    let d = tempfile::tempdir().unwrap();
    fixture_tree(d.path());
    // 2: usage
    assert_eq!(code(&omnizip(d.path(), &["x"])), 2);
    // 3: missing archive
    let o = omnizip(d.path(), &["l", "missing.zip"]);
    assert_eq!(code(&o), 3);
    assert!(stderr(&o).contains("error[E1002]"), "{}", stderr(&o));
    // 4: not an archive
    fs::write(d.path().join("note.txt"), "text").unwrap();
    assert_eq!(code(&omnizip(d.path(), &["l", "note.txt"])), 4);
    // 6: encrypted, no password, no prompting allowed
    assert_eq!(
        code(&omnizip(
            d.path(),
            &["a", "enc.7z", "proj", "--password=pw", "--encrypt-headers"]
        )),
        0
    );
    assert_eq!(code(&omnizip(d.path(), &["l", "enc.7z"])), 6);
    let o = omnizip(d.path(), &["l", "enc.7z", "--password=bad"]);
    assert_eq!(code(&o), 6);
    assert_eq!(
        code(&omnizip(d.path(), &["l", "enc.7z", "--password=pw"])),
        0
    );
    // 7: destination archive exists and is not updatable
    assert_eq!(code(&omnizip(d.path(), &["a", "enc.7z", "proj"])), 7);
}

#[test]
fn json_errors_are_structured() {
    let d = tempfile::tempdir().unwrap();
    let o = omnizip(d.path(), &["l", "missing.zip", "--json"]);
    let line = stderr(&o);
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["code"], "E1002");
    assert_eq!(v["error"]["key"], "not-found");
}

#[test]
fn password_file_and_env() {
    let d = tempfile::tempdir().unwrap();
    fixture_tree(d.path());
    fs::write(d.path().join("pw.txt"), "s3cret\n").unwrap();
    assert_eq!(
        code(&omnizip(
            d.path(),
            &["a", "e.zip", "proj", "--password-file", "pw.txt"]
        )),
        0
    );
    let o = Command::new(env!("CARGO_BIN_EXE_omnizip"))
        .current_dir(d.path())
        .args([
            "x",
            "e.zip",
            "-o",
            "out",
            "--password-env",
            "OZ_PW",
            "--no-input",
            "--lang",
            "en-US",
        ])
        .env("OZ_PW", "s3cret")
        .output()
        .unwrap();
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(d.path().join("out/proj/src/main.rs").is_file());
}

#[test]
fn non_interactive_conflicts_skip_and_yes_overwrites() {
    let d = tempfile::tempdir().unwrap();
    fixture_tree(d.path());
    assert_eq!(code(&omnizip(d.path(), &["a", "p.zip", "proj"])), 0);
    assert_eq!(code(&omnizip(d.path(), &["x", "p.zip", "-o", "out"])), 0);
    fs::write(d.path().join("out/proj/读我.md"), "changed").unwrap();
    let o = omnizip(d.path(), &["x", "p.zip", "-o", "out"]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        fs::read_to_string(d.path().join("out/proj/读我.md")).unwrap(),
        "changed"
    );
    let o = omnizip(d.path(), &["x", "p.zip", "-o", "out", "-y"]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        fs::read_to_string(d.path().join("out/proj/读我.md")).unwrap(),
        "# 你好\n"
    );
}

#[test]
fn include_exclude_and_stdout() {
    let d = tempfile::tempdir().unwrap();
    fixture_tree(d.path());
    assert_eq!(code(&omnizip(d.path(), &["a", "p.7z", "proj"])), 0);
    let o = omnizip(d.path(), &["x", "p.7z", "--stdout", "--include", "*.md"]);
    assert_eq!(stdout(&o), "# 你好\n");
    let o = omnizip(
        d.path(),
        &["x", "p.7z", "-o", "only-src", "--exclude", "*.md"],
    );
    assert_eq!(code(&o), 0);
    assert!(d.path().join("only-src/proj/src/main.rs").is_file());
    assert!(!d.path().join("only-src/proj/读我.md").exists());
}

#[test]
fn localized_help() {
    let o = Command::new(env!("CARGO_BIN_EXE_omnizip"))
        .args(["x", "--help", "--lang", "zh-CN"])
        .output()
        .unwrap();
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("解压"), "{}", stdout(&o));
}

#[test]
fn completions_and_formats() {
    let d = tempfile::tempdir().unwrap();
    let o = omnizip(d.path(), &["completions", "powershell"]);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("omnizip"));
    let o = omnizip(d.path(), &["formats", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    let zip = v
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["format"] == "zip")
        .unwrap();
    assert!(
        zip["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "WRITE")
    );
}
