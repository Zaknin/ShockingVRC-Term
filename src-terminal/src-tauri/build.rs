use std::{path::PathBuf, process::Command};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let repo = manifest_dir.join("../..");
    let head = git_path(&repo, &["rev-parse", "--git-path", "HEAD"]);
    if let Some(path) = head.as_deref() {
        println!("cargo:rerun-if-changed={}", repo.join(path).display());
    }
    if let Some(branch) = git_output(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        if let Some(reference) = git_path(&repo, &["rev-parse", "--git-path", &format!("refs/heads/{branch}")]) {
            println!("cargo:rerun-if-changed={}", repo.join(reference).display());
        }
    }
    let commit = git_output(&repo, &["rev-parse", "--verify", "HEAD^{commit}"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=SHOCKINGVRC_GIT_COMMIT={commit}");
    tauri_build::build()
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_path(repo: &std::path::Path, args: &[&str]) -> Option<PathBuf> {
    git_output(repo, args).map(PathBuf::from)
}
