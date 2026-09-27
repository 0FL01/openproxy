use std::path::Path;
use std::process::{Command, Output};

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn run_script(binary: &Path, directory: &Path, commit: Option<&str>) -> Output {
    let mut command = Command::new(binary);
    command
        .current_dir(directory)
        .env_remove("CARGO_FEATURE_EMBED_WEB")
        .env_remove("OPENPROXY_BUILD_COMMIT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    if let Some(commit) = commit {
        command.env("OPENPROXY_BUILD_COMMIT", commit);
    }
    command.output().unwrap()
}

#[test]
fn build_script_validates_overrides_and_tracks_git_worktrees() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("build-script");
    let status = Command::new("rustc")
        .args([
            "--edition=2024",
            concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"),
            "-o",
        ])
        .arg(&binary)
        .status()
        .unwrap();
    assert!(status.success());

    let unknown = run_script(&binary, temp.path(), None);
    assert!(unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stdout)
        .lines()
        .any(|line| line == "cargo:rustc-env=OPENPROXY_BUILD_COMMIT="));
    for commit in ["a".repeat(40), "F".repeat(64)] {
        let output = run_script(&binary, temp.path(), Some(&commit));
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout)
            .contains(&format!("cargo:rustc-env=OPENPROXY_BUILD_COMMIT={commit}")));
    }
    for invalid in ["abc12345", &"g".repeat(40), &"a".repeat(41)] {
        assert!(!run_script(&binary, temp.path(), Some(invalid))
            .status
            .success());
    }

    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "test",
        ],
    );
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let output = run_script(&binary, &repo, None);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("cargo:rustc-env=OPENPROXY_BUILD_COMMIT={sha}")));
    for name in ["HEAD", "packed-refs", "refs"] {
        let path = git(&repo, &["rev-parse", "--git-path", name]);
        assert!(stdout.contains(&format!("cargo:rerun-if-changed={path}")));
    }
    let reference = git(&repo, &["symbolic-ref", "HEAD"]);
    let path = git(&repo, &["rev-parse", "--git-path", &reference]);
    assert!(stdout.contains(&format!("cargo:rerun-if-changed={path}")));

    let worktree = temp.path().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            "test-worktree",
            worktree.to_str().unwrap(),
        ],
    );
    let output = run_script(&binary, &worktree, None);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("cargo:rustc-env=OPENPROXY_BUILD_COMMIT={sha}")));
    let head = git(&worktree, &["rev-parse", "--git-path", "HEAD"]);
    assert!(stdout.contains(&format!("cargo:rerun-if-changed={head}")));
    let reference = git(
        &worktree,
        &["rev-parse", "--git-path", "refs/heads/test-worktree"],
    );
    assert!(stdout.contains(&format!("cargo:rerun-if-changed={reference}")));
}
