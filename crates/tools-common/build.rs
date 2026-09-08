use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest_dir.join("../..");

    emit_git_rerun_paths(&repository);

    let status_trigger = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("git-status-trigger");
    println!("cargo:rerun-if-changed={}", status_trigger.display());
    println!("cargo:rustc-check-cfg=cfg(haucet_git_dirty)");
    if git_output(
        &repository,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )
    .is_some()
    {
        println!("cargo:rustc-cfg=haucet_git_dirty");
    }

    let package_version = env::var("CARGO_PKG_VERSION").unwrap();
    let tag = git_output(&repository, &["describe", "--tags", "--abbrev=0"]);
    let commit =
        git_output(&repository, &["rev-parse", "HEAD"]).and_then(|hash| commit_suffix(&hash));
    let base_version = tag.as_deref().unwrap_or(&package_version);
    let version = match &commit {
        Some(commit) => format!("{base_version} ({commit})"),
        None => base_version.to_owned(),
    };

    if let Some(tag) = tag {
        println!("cargo:rustc-env=HAUCET_GIT_TAG={tag}");
    }
    if let Some(commit) = commit {
        println!("cargo:rustc-env=HAUCET_GIT_COMMIT={commit}");
    }
    println!("cargo:rustc-env=HAUCET_VERSION={version}");
}

fn git_output(repository: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn commit_suffix(hash: &str) -> Option<String> {
    if hash.len() < 8 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash[hash.len() - 8..].to_owned())
}

fn emit_git_rerun_paths(repository: &Path) {
    for name in ["HEAD", "logs/HEAD", "packed-refs", "refs/tags"] {
        emit_git_path(repository, name);
    }
    if let Some(reference) = git_output(repository, &["symbolic-ref", "-q", "HEAD"]) {
        emit_git_path(repository, &reference);
    }
}

fn emit_git_path(repository: &Path, name: &str) {
    let Some(path) = git_output(repository, &["rev-parse", "--git-path", name]) else {
        return;
    };
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        repository.join(path)
    };
    println!("cargo:rerun-if-changed={}", path.display());
}
