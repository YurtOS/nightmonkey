use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile(snapshot: &Path, scratch: &Path, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nightmonkey"))
        .current_dir(scratch)
        .args(options)
        .arg(snapshot)
        .args(["-o", "output.wasm"])
        .output()
        .expect("run the actual compiler CLI")
}

fn diagnostics(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// Run explicitly with a snapshot captured from the matching engine/runtime.
// An absent fixture fails the qualification run instead of skipping it.
#[test]
#[ignore = "requires NIGHT_PIPELINE_FIXTURE from a matching integrated build"]
fn pipeline_selection_compiles_and_reports_the_requested_tier() {
    let snapshot = std::fs::canonicalize(
        std::env::var_os("NIGHT_PIPELINE_FIXTURE").expect("NIGHT_PIPELINE_FIXTURE is required"),
    )
    .expect("captured fixture exists");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "nightmonkey-pipeline-{}-{nonce}",
        std::process::id()
    )));
    std::fs::create_dir(&scratch.0).unwrap();

    let control = compile(&snapshot, &scratch.0, &[]);
    assert!(control.status.success(), "{}", diagnostics(&control));

    for pipeline in ["mir", "baseline"] {
        std::fs::remove_file(scratch.0.join("output.wasm")).unwrap();
        let output = compile(
            &snapshot,
            &scratch.0,
            &["--pipeline", pipeline, "--dump-tiers", "--strict-coverage"],
        );
        let report = diagnostics(&output);
        assert!(output.status.success(), "{pipeline}: {report}");
        assert!(
            report.lines().any(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                fields.len() >= 4
                    && fields[0] == "night:"
                    && fields[1] == "tier"
                    && fields[3] == pipeline
            }),
            "missing positive {pipeline} script witness: {report}"
        );
        let module = std::fs::read(scratch.0.join("output.wasm")).unwrap();
        assert!(module.starts_with(b"\0asm") && module.len() > 8);
    }

    let invalid = compile(&snapshot, &scratch.0, &["--pipeline", "legacy"]);
    assert!(!invalid.status.success());
    assert!(diagnostics(&invalid).contains("pipeline"));
}
