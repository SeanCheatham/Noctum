//! Git worktrees. The user's checkout is never the directory a commit, apply,
//! or reset runs in.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tempfile::NamedTempFile;

use crate::process;
use crate::PERF_BRANCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerfSync {
    /// `noctum/perf` does not exist. The night starts at the user's HEAD.
    Fresh,
    /// The user's HEAD is an ancestor of `noctum/perf`.
    Ahead,
    /// Rebase onto the user's HEAD succeeded.
    Rebased,
    Conflict(String),
}

pub async fn sync_perf(repo: &Path, head: &str, dest: &Path) -> Result<PerfSync> {
    if dest.exists() {
        let _ = remove_worktree(repo, dest).await;
    }
    if !rev_exists(repo, PERF_BRANCH).await? {
        add_detached(repo, dest, head).await?;
        return Ok(PerfSync::Fresh);
    }
    if is_ancestor(repo, head, PERF_BRANCH).await? {
        add_branch(repo, dest, PERF_BRANCH).await?;
        return Ok(PerfSync::Ahead);
    }
    add_branch(repo, dest, PERF_BRANCH).await?;
    let rebase = git(dest, &["rebase", head]).await?;
    if rebase.success {
        return Ok(PerfSync::Rebased);
    }
    let detail = crate::hashutil::tail_str(&rebase.combined(), crate::LOG_LIMIT);
    let _ = git(dest, &["rebase", "--abort"]).await;
    Ok(PerfSync::Conflict(detail))
}

pub async fn head_sha(repo: &Path) -> Result<String> {
    let output = git(repo, &["rev-parse", "HEAD"]).await?;
    if !output.success {
        bail!("git rev-parse HEAD failed: {}", output.stderr);
    }
    Ok(output.stdout.trim().to_string())
}

pub async fn status_porcelain(worktree: &Path) -> Result<String> {
    let output = git(worktree, &["status", "--porcelain"]).await?;
    if !output.success {
        bail!("git status failed: {}", output.stderr);
    }
    Ok(output.stdout.to_string())
}

pub async fn checkout_files(worktree: &Path, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["checkout".to_string(), "--".into()];
    args.extend(paths.iter().cloned());
    let output = git(worktree, &args).await?;
    if !output.success
        && !output.stderr.contains("did not match any file")
        && !output.stderr.contains("pathspec")
    {
        bail!("git checkout failed: {}", output.stderr);
    }
    Ok(())
}

pub async fn restore_tracked(worktree: &Path, clean_paths: &[String]) -> Result<()> {
    let checkout = git(worktree, &["checkout", "--", "."]).await?;
    if !checkout.success {
        bail!("git checkout failed: {}", checkout.stderr);
    }
    clean(worktree, clean_paths).await
}

pub async fn reset_hard(worktree: &Path, sha: &str) -> Result<()> {
    let output = git(worktree, &["reset", "--hard", sha]).await?;
    if !output.success {
        bail!("git reset failed: {}", output.stderr);
    }
    Ok(())
}

pub async fn clean(worktree: &Path, clean_paths: &[String]) -> Result<()> {
    let mut args = vec!["clean".to_string(), "-fd".into()];
    for path in clean_paths {
        args.push("-e".into());
        args.push(path.clone());
    }
    let output = git(worktree, &args).await?;
    if !output.success {
        bail!("git clean failed: {}", output.stderr);
    }
    Ok(())
}

pub async fn apply_check(worktree: &Path, diff: &str) -> Result<()> {
    apply(worktree, diff, true).await
}

pub async fn apply(worktree: &Path, diff: &str, check_only: bool) -> Result<()> {
    let mut file = NamedTempFile::new().context("diff tempfile")?;
    std::io::Write::write_all(&mut file, diff.as_bytes())?;
    let path = file.path().to_string_lossy().to_string();
    let args: Vec<&str> = if check_only {
        vec!["apply", "--check", &path]
    } else {
        vec!["apply", &path]
    };
    let output = git(worktree, &args).await?;
    if !output.success {
        bail!("{}", output.combined());
    }
    Ok(())
}

pub async fn commit(
    worktree: &Path,
    paths: &[String],
    message: &str,
    author_name: &str,
    author_email: &str,
) -> Result<String> {
    if paths.is_empty() {
        bail!("commit has no paths");
    }
    let mut add = vec!["add".to_string(), "--".into()];
    add.extend(paths.iter().cloned());
    let added = git(worktree, &add).await?;
    if !added.success {
        bail!("git add failed: {}", added.stderr);
    }
    let mut message_file = NamedTempFile::new()?;
    std::io::Write::write_all(&mut message_file, message.as_bytes())?;
    let message_path = message_file.path().to_string_lossy().to_string();
    let name = format!("user.name={author_name}");
    let email = format!("user.email={author_email}");
    let output = git(
        worktree,
        &["-c", &name, "-c", &email, "commit", "-F", &message_path],
    )
    .await?;
    if !output.success {
        bail!("git commit failed: {}", output.combined());
    }
    ensure_perf_branch(worktree).await?;
    head_sha(worktree).await
}

async fn ensure_perf_branch(worktree: &Path) -> Result<()> {
    let current = git(worktree, &["branch", "--show-current"]).await?;
    if current.stdout.trim() == PERF_BRANCH {
        return Ok(());
    }
    let exists = git(
        worktree,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{PERF_BRANCH}"),
        ],
    )
    .await?;
    let switched = if exists.success {
        let updated = git(worktree, &["branch", "-f", PERF_BRANCH, "HEAD"]).await?;
        if !updated.success {
            bail!("git branch -f failed: {}", updated.stderr);
        }
        git(worktree, &["checkout", PERF_BRANCH]).await?
    } else {
        git(worktree, &["checkout", "-b", PERF_BRANCH]).await?
    };
    if !switched.success {
        bail!(
            "failed to put the commit on {PERF_BRANCH}: {}",
            switched.combined()
        );
    }
    Ok(())
}

pub async fn file_blob(worktree: &Path, path: &str) -> Result<String> {
    let output = git(worktree, &["hash-object", "--", path]).await?;
    if !output.success {
        bail!("git hash-object failed: {}", output.stderr);
    }
    Ok(output.stdout.trim().to_string())
}

pub async fn show_file(repo: &Path, rev: &str, path: &str) -> Result<String> {
    let spec = format!("{rev}:{path}");
    let output = git(repo, &["show", &spec]).await?;
    if !output.success {
        bail!("git show failed: {}", output.stderr);
    }
    Ok(output.stdout)
}

async fn add_detached(repo: &Path, dest: &Path, rev: &str) -> Result<()> {
    let path = dest.to_string_lossy().to_string();
    let output = git(repo, &["worktree", "add", "--detach", &path, rev]).await?;
    if !output.success {
        bail!("git worktree add failed: {}", output.combined());
    }
    Ok(())
}

async fn add_branch(repo: &Path, dest: &Path, branch: &str) -> Result<()> {
    let path = dest.to_string_lossy().to_string();
    let output = git(repo, &["worktree", "add", &path, branch]).await?;
    if !output.success {
        bail!("git worktree add failed: {}", output.combined());
    }
    Ok(())
}

async fn remove_worktree(repo: &Path, dest: &Path) -> Result<()> {
    let path = dest.to_string_lossy().to_string();
    let _ = git(repo, &["worktree", "remove", "--force", &path]).await;
    if dest.exists() {
        std::fs::remove_dir_all(dest).ok();
        let _ = git(repo, &["worktree", "prune"]).await;
    }
    Ok(())
}

pub async fn remove(repo: &Path, dest: &Path) -> Result<()> {
    remove_worktree(repo, dest).await
}

async fn rev_exists(repo: &Path, rev: &str) -> Result<bool> {
    let output = git(repo, &["rev-parse", "--verify", "--quiet", rev]).await?;
    Ok(output.success)
}

async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = git(repo, &["merge-base", "--is-ancestor", ancestor, descendant]).await?;
    Ok(output.success)
}

async fn git(
    cwd: &Path,
    args: &[impl AsRef<std::ffi::OsStr> + Send + Sync],
) -> Result<process::CmdOutput> {
    Ok(process::run(cwd, "git", args, &[], Duration::from_secs(120)).await)
}

pub fn diff_paths(diff: &str) -> Vec<String> {
    crate::diffcheck::parse_diff(diff)
        .map(|files| files.into_iter().map(|file| file.path).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init"]).await.unwrap();
        git(path, &["config", "user.email", "test@example.com"])
            .await
            .unwrap();
        git(path, &["config", "user.name", "Test"]).await.unwrap();
        std::fs::write(path.join("src.txt"), "one\n").unwrap();
        git(path, &["add", "src.txt"]).await.unwrap();
        git(path, &["commit", "-m", "init"]).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn apply_and_commit_stay_out_of_the_original_checkout() {
        let repo = init_repo().await;
        let head = head_sha(repo.path()).await.unwrap();
        let dest = tempfile::tempdir().unwrap();
        let wt = dest.path().join("wt");
        let sync = sync_perf(repo.path(), &head, &wt).await.unwrap();
        assert_eq!(sync, PerfSync::Fresh);
        let diff = "diff --git a/src.txt b/src.txt\n--- a/src.txt\n+++ b/src.txt\n@@ -1 +1 @@\n-one\n+two\n";
        apply(&wt, diff, false).await.unwrap();
        let sha = commit(
            &wt,
            &["src.txt".into()],
            "faster\n\nTest: t\n",
            "Noctum",
            "noctum@localhost",
        )
        .await
        .unwrap();
        assert_ne!(sha, head);
        assert_eq!(head_sha(repo.path()).await.unwrap(), head);
        assert!(status_porcelain(repo.path()).await.unwrap().is_empty());
        let shown = show_file(&wt, &sha, "src.txt").await.unwrap();
        assert_eq!(shown, "two\n");
    }
}
