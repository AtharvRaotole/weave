use std::fs;
use std::process::Command;

fn weave(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_weave"))
        .args(args)
        .output()
        .expect("run weave");
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn check_run_replay_and_trace() {
    let dir = std::env::temp_dir().join(format!("weave-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("hello.weave");
    let trace = dir.join("trace.json");
    fs::write(&script, "temp = weather(\"Paris\")\nprint(temp)\n").unwrap();

    let (ok, stdout, stderr) = weave(&[
        "check",
        script.to_str().unwrap(),
        "--tool",
        "name=weather,effect=read,arity=1",
    ]);
    assert!(ok, "{stderr}");
    assert_eq!(stdout, "ok\n");

    let (ok, stdout, stderr) = weave(&[
        "run",
        script.to_str().unwrap(),
        "--tool",
        "name=weather,effect=read,behavior=echo,arity=1",
        "--trace-out",
        trace.to_str().unwrap(),
    ]);
    assert!(ok, "{stderr}");
    assert_eq!(stdout, "Paris\n");

    let (ok, viewed, stderr) = weave(&["trace", trace.to_str().unwrap()]);
    assert!(ok, "{stderr}");
    assert!(viewed.contains("weather"), "{viewed}");
    assert!(viewed.contains("Paris"), "{viewed}");

    let (ok, replayed, stderr) = weave(&[
        "replay",
        script.to_str().unwrap(),
        "--trace",
        trace.to_str().unwrap(),
        "--tool",
        "name=weather,effect=read,arity=1",
    ]);
    assert!(ok, "{stderr}");
    assert_eq!(replayed, "Paris\n");
    let _ = fs::remove_dir_all(&dir);
}
