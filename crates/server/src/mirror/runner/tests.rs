use std::time::Instant;

use monitorflow_sensors::testing::write_executable;

use super::*;

/// A directory with one script per tool, standing in for the real
/// programs. Production never runs a shell.
struct FakeTools {
    dir: tempfile::TempDir,
}

impl FakeTools {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for tool in ["aptly", "reposync", "createrepo_c", "gpg"] {
            write_executable(
                &dir.path().join(tool),
                &format!("#!/bin/sh\n{script}\n"),
                0o755,
            );
        }
        Self { dir }
    }

    fn runner(&self, timeout: u64) -> CommandRunner {
        let config = MirrorConfig {
            aptly: self.dir.path().join("aptly"),
            reposync: self.dir.path().join("reposync"),
            createrepo_c: self.dir.path().join("createrepo_c"),
            gpg: self.dir.path().join("gpg"),
            sync_timeout: timeout,
            max_concurrent_syncs: 1,
        };
        CommandRunner::new(&config, self.dir.path().join("gnupg"))
    }
}

#[tokio::test]
async fn arguments_pass_without_a_shell_in_a_clean_environment() {
    let fake = FakeTools::new(
        r#"echo "tool=$(basename "$0")"; for a in "$@"; do echo "[$a]"; done
echo "[$LC_ALL][$GNUPGHOME][${DATABASE_URL-unset}][$(pwd)]"; echo warn >&2"#,
    );
    let cwd = tempfile::tempdir().unwrap();
    let invocation = Invocation::new(Tool::Reposync, ["a b", "$(x)", "--x=*"]).in_dir(cwd.path());
    let out = fake.runner(10).run(&invocation).await.unwrap();
    assert!(out.success);
    let gnupg = fake.dir.path().join("gnupg");
    let cwd = cwd.path().canonicalize().unwrap();
    assert_eq!(
        out.stdout,
        format!(
            "tool=reposync\n[a b]\n[$(x)]\n[--x=*]\n[C][{}][unset][{}]\n",
            gnupg.display(),
            cwd.display()
        )
    );
    assert_eq!(out.stderr, "warn\n");
    assert_eq!(out.combined(), format!("{}warn\n", out.stdout));
    assert_eq!(invocation.to_string(), "reposync 'a b' $(x) --x=*");
}

#[tokio::test]
async fn failures_keep_the_output() {
    let fake =
        FakeTools::new("echo progress; echo 'first' >&2; echo 'ERROR: disk full' >&2; exit 3");
    let invocation = Invocation::new(Tool::Aptly, ["mirror", "update", "x"]);
    let out = fake.runner(10).run(&invocation).await.unwrap();
    assert!(!out.success);
    let error = out.check(&invocation).unwrap_err();
    assert_eq!(
        error.to_string(),
        "aptly failed (exit status: 3): ERROR: disk full"
    );
    assert_eq!(error.output().unwrap().stdout, "progress\n");

    let missing = CommandRunner::new(
        &MirrorConfig {
            gpg: fake.dir.path().join("nope"),
            ..MirrorConfig::default()
        },
        fake.dir.path(),
    );
    let error = missing
        .run(&Invocation::new(Tool::Gpg, ["--version"]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, RunError::Spawn { tool: "gpg", .. }),
        "{error}"
    );
}

#[tokio::test]
async fn timeouts_kill_the_whole_process_group() {
    // The background sleep holds stdout open: without killing the group
    // the reader would wait for it.
    let fake = FakeTools::new("sleep 30 & sleep 30");
    let started = Instant::now();
    let error = fake
        .runner(1)
        .run(&Invocation::new(Tool::CreaterepoC, ["."]))
        .await
        .unwrap_err();
    assert!(matches!(error, RunError::Timeout { .. }), "{error}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn output_keeps_its_tail() {
    // 200,005 bytes in all.
    let fake = FakeTools::new("head -c 200000 /dev/zero | tr '\\0' a; echo; echo END");
    let out = fake
        .runner(10)
        .run(&Invocation::new(Tool::Aptly, ["x"]))
        .await
        .unwrap();
    assert!(
        out.stdout.starts_with("[134469 bytes omitted]\n"),
        "{}",
        &out.stdout[..40]
    );
    assert!(out.stdout.ends_with("a\nEND\n"));
    assert!(out.stdout.len() < OUTPUT_TAIL + 64);
}

#[test]
fn keep_tail_cuts_at_a_character_boundary() {
    assert_eq!(keep_tail("short", 10), "short");
    assert_eq!(keep_tail("abcdef", 3), "[3 bytes omitted]\ndef");
    // `é` is two bytes; the cut moves past it rather than splitting it.
    assert_eq!(keep_tail("aéb", 2), "[3 bytes omitted]\nb");
}
