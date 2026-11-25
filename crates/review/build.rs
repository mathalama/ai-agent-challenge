use std::{path::Path, process::Command};

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let result = Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    let output = String::from_utf8(result.stdout).ok()?;
    let output = output.trim();
    (!output.is_empty()).then(|| output.to_owned())
}

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("../..");
    println!("cargo:rerun-if-changed=build.rs");
    for file in ["HEAD", "index", "packed-refs", "refs/tags"] {
        if let Some(path) = git(&workspace, &["rev-parse", "--git-path", file]) {
            let path = workspace.join(path);
            let watched = if path.exists() {
                Some(path.as_path())
            } else {
                path.parent()
            };
            if let Some(watched) = watched.filter(|path| path.exists()) {
                println!("cargo:rerun-if-changed={}", watched.display());
            }
        }
    }
    if let Some(files) = git(&workspace, &["ls-files", "-z", "--cached"]) {
        for file in files.split_terminator('\0') {
            println!("cargo:rerun-if-changed={}", workspace.join(file).display());
        }
    }
    if let Some(head_ref) = git(&workspace, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&workspace, &["rev-parse", "--git-path", &head_ref])
    {
        let path = workspace.join(path);
        for ancestor in path.ancestors() {
            if ancestor.exists() {
                println!("cargo:rerun-if-changed={}", ancestor.display());
                break;
            }
        }
    }
    let commit =
        git(&workspace, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let mut version = match git(&workspace, &["describe", "--tags", "--exact-match", "HEAD"]) {
        Some(tag) => format!("{commit} ({tag})"),
        None => commit,
    };
    if git(
        &workspace,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )
    .is_some()
    {
        version.push_str("-dirty");
    }
    println!("cargo:rustc-env=TZ_AGENT_VERSION={version}");
}
