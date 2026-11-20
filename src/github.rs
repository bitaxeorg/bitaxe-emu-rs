//! GitHub Actions artifacts: the latest run of a workflow on a branch that
//! holds a given artifact, its download, and files of the repository at the
//! run's commit.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

const API: &str = "https://api.github.com";
/// Workflow runs looked at for an artifact, newest first.
const RUNS: usize = 20;

pub struct Github {
    agent: ureq::Agent,
    token: Option<String>,
}

/// A workflow run.
#[derive(Clone, Debug)]
pub struct Run {
    pub id: u64,
    pub sha: String,
    /// Creation date, `YYYY-MM-DD`.
    pub date: String,
}

/// The token for the API: `GH_TOKEN`, `GITHUB_TOKEN`, else the one of the
/// GitHub CLI (`gh auth token`).
fn token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(token) = std::env::var(var)
            && !token.trim().is_empty()
        {
            return Some(token.trim().to_string());
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}

impl Github {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(300)))
            .user_agent("bitaxe-emu")
            .build()
            .into();
        Github {
            agent,
            token: token(),
        }
    }

    fn get(&self, url: &str, accept: &str) -> Result<Vec<u8>, String> {
        let mut request = self
            .agent
            .get(url)
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request.call().map_err(|e| format!("{url}: {e}"))?;
        response
            .body_mut()
            .with_config()
            .limit(1 << 30)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))
    }

    fn json(&self, path: &str) -> Result<Value, String> {
        let bytes = self.get(&format!("{API}{path}"), "application/vnd.github+json")?;
        serde_json::from_slice(&bytes).map_err(|e| format!("{API}{path}: {e}"))
    }

    /// The newest run of `workflow` pushed to `branch` that holds `artifact`,
    /// with the artifact's id.
    pub fn latest_artifact(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
        artifact: &str,
    ) -> Result<(Run, u64), String> {
        let runs = self.json(&format!(
            "/repos/{repo}/actions/workflows/{workflow}/runs?branch={branch}&event=push&status=completed&per_page={RUNS}"
        ))?;
        for run in runs["workflow_runs"].as_array().into_iter().flatten() {
            let Some(id) = run["id"].as_u64() else {
                continue;
            };
            let artifacts = self.json(&format!(
                "/repos/{repo}/actions/runs/{id}/artifacts?name={artifact}"
            ))?;
            let found = artifacts["artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["expired"] == false)
                .and_then(|a| a["id"].as_u64());
            if let Some(artifact_id) = found {
                let run = Run {
                    id,
                    sha: run["head_sha"].as_str().unwrap_or_default().to_string(),
                    date: run["created_at"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(10)
                        .collect(),
                };
                return Ok((run, artifact_id));
            }
        }
        Err(format!(
            "no {artifact} artifact in the last {RUNS} runs of {repo} {workflow} on {branch}"
        ))
    }

    /// The zip of an artifact. GitHub serves them to authenticated clients
    /// only.
    pub fn download_artifact(&self, repo: &str, id: u64) -> Result<Vec<u8>, String> {
        if self.token.is_none() {
            return Err(
                "downloading a CI artifact needs a GitHub token: set GH_TOKEN, or log in with `gh auth login`"
                    .into(),
            );
        }
        self.get(
            &format!("{API}/repos/{repo}/actions/artifacts/{id}/zip"),
            "application/vnd.github+json",
        )
    }

    /// A file of the repository at commit `sha`.
    pub fn file(&self, repo: &str, sha: &str, path: &str) -> Result<Vec<u8>, String> {
        self.get(
            &format!("{API}/repos/{repo}/contents/{path}?ref={sha}"),
            "application/vnd.github.raw",
        )
    }
}

/// Extracts the files of `zip` under `dir`.
pub fn unzip(zip: &[u8], dir: &Path) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| format!("artifact: {e}"))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| format!("artifact: {e}"))?;
        let Some(name) = file.enclosed_name() else {
            continue;
        };
        let path = dir.join(name);
        if file.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut out =
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        std::io::copy(&mut file, &mut out).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Where downloaded firmware is kept: `$XDG_CACHE_HOME/bitaxe-emu`, else
/// `~/.cache/bitaxe-emu`.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("bitaxe-emu")
}
