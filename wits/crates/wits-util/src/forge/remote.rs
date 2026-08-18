//! Making sense of a git remote URL.
//!
//! Remote URLs come in three shapes that all mean the same thing — scp-style
//! (`git@host:owner/repo.git`), a real URI (`https://host/owner/repo.git`,
//! `ssh://git@host:22/owner/repo`), and either of those hidden behind an SSH
//! host alias from `~/.ssh/config`. Everything downstream — which forge to call,
//! who owns the fork — needs the same three facts out of that mess: the real
//! host, the owner, and the repo. Pulling those out reliably, alias included, is
//! the entire job of this module.
//!
//! Parsing is kept pure (no git, no config) so it is trivially testable; the one
//! impurity, resolving an SSH alias, is isolated and only reached for the URL
//! forms that can actually carry one.

use crate::git::Repository;
use crate::process::Command;

/// A git hosting service. `Unknown` is a first-class outcome, not a failure: a
/// self-hosted instance behind a custom domain parses fine, and the forge layer
/// can still be told what it is by config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    GitHub,
    GitLab,
    /// Gitea — the original of the Gitea/Forgejo API family.
    Gitea,
    /// Forgejo, Gitea's hard fork.
    Forgejo,
    /// codeberg.org specifically — a hosted Forgejo instance.
    Codeberg,
    Bitbucket,
    Unknown,
}

impl Service {
    /// The lowercase name used in config keys (`wits.forge.<name>.token`)
    /// and as the value of a `.service` override.
    pub fn as_str(self) -> &'static str {
        match self {
            Service::GitHub => "github",
            Service::GitLab => "gitlab",
            Service::Gitea => "gitea",
            Service::Forgejo => "forgejo",
            Service::Codeberg => "codeberg",
            Service::Bitbucket => "bitbucket",
            Service::Unknown => "unknown",
        }
    }

    pub fn parse(name: &str) -> Option<Service> {
        Some(match name.trim().to_lowercase().as_str() {
            "github" => Service::GitHub,
            "gitlab" => Service::GitLab,
            "gitea" => Service::Gitea,
            "forgejo" => Service::Forgejo,
            "codeberg" => Service::Codeberg,
            "bitbucket" => Service::Bitbucket,
            _ => return None,
        })
    }
}

/// The three facts about a remote, plus the host's best-guess service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub service: Service,
}

impl RemoteInfo {
    /// `owner/repo`, the form most forge APIs want in a path.
    pub fn project_path(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// A few hosts publish a dedicated SSH endpoint that is the same service under a
/// different name; collapse those so detection keys off one canonical domain.
fn normalize_domain(domain: &str) -> String {
    let domain = domain.to_lowercase();
    match domain.as_str() {
        "ssh.github.com" => "github.com",
        "altssh.gitlab.com" => "gitlab.com",
        "altssh.bitbucket.org" => "bitbucket.org",
        other => other,
    }
    .to_owned()
}

fn detect_service(domain: &str) -> Service {
    match domain {
        "github.com" | "www.github.com" => Service::GitHub,
        "bitbucket.org" | "www.bitbucket.org" => Service::Bitbucket,
        "gitlab.com" | "www.gitlab.com" => Service::GitLab,
        "codeberg.org" | "www.codeberg.org" => Service::Codeberg,
        // Self-hosted instances conventionally keep the product in the hostname,
        // which is the only signal we have for them.
        d if d.contains("gitlab") => Service::GitLab,
        d if d.contains("forgejo") => Service::Forgejo,
        d if d.contains("gitea") => Service::Gitea,
        _ => Service::Unknown,
    }
}

/// Resolve an SSH host alias to its real hostname.
///
/// The alias→host mapping lives in the user's SSH config, whose real semantics —
/// wildcards, `Match` blocks, `Include` directives, token expansion — are a
/// small language in their own right. Parsing it ourselves would be a second
/// implementation of ssh's logic that inevitably drifts from it, and the whole
/// reason we need the answer is to match what ssh (and therefore git) will do.
/// So we ask ssh: `ssh -G` prints the fully resolved config and never opens a
/// connection, which makes it both authoritative and cheap. A bare domain
/// resolves to itself, so running this on any SSH-bearing URL is harmless.
fn resolve_ssh_alias(host: &str) -> String {
    let Ok(result) = Command::new("ssh").args(["-G", host]).force_run().exec() else {
        return host.to_owned();
    };
    if !result.is_success() {
        return host.to_owned();
    }
    for line in result.stdout.lines() {
        if let Some(rest) = line
            .strip_prefix("hostname ")
            .or_else(|| line.strip_prefix("HostName "))
        {
            let name = rest.trim();
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    host.to_owned()
}

/// Split a remote URL into `(raw_host, path, carries_ssh)`. `carries_ssh` marks
/// the forms where a host alias is worth resolving; for `https` it is pointless.
fn split_url(url: &str) -> Option<(String, String, bool)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    if let Some(after_scheme) = url.split_once("://") {
        // Real URI: scheme://[user@]host[:port]/path
        let (scheme, rest) = after_scheme;
        let (authority, path) = rest.split_once('/')?;
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let host = host_port.split(':').next().unwrap_or(host_port);
        let carries_ssh = scheme == "ssh" || scheme == "git";
        return Some((host.to_owned(), path.to_owned(), carries_ssh));
    }

    // scp-style: [user@]host:path — distinguished from a URI by having no
    // scheme. The colon separates host from path, so split on the first one.
    let (left, path) = url.split_once(':')?;
    let host = left.rsplit('@').next().unwrap_or(left);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((host.to_owned(), path.to_owned(), true))
}

/// Parse a remote URL into its host, owner, repo, and detected service. Returns
/// `None` for anything that doesn't carry an `owner/repo` pair.
pub fn parse_url(url: &str) -> Option<RemoteInfo> {
    let (raw_host, path, carries_ssh) = split_url(url)?;

    let host = if carries_ssh {
        normalize_domain(&resolve_ssh_alias(&raw_host))
    } else {
        normalize_domain(&raw_host)
    };

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.rsplit_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    let service = detect_service(&host);
    Some(RemoteInfo {
        host,
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        service,
    })
}

/// The two remotes that carry meaning for a stack, resolved once from the repo.
///
/// `origin` is where we push and the head side of every MR; `upstream` is the
/// merge target when we are working on a fork. The whole point of naming the
/// roles is that the rest of the tool never has to re-derive "which remote does
/// the MR go against" — it asks [`target`](Remotes::target).
#[derive(Debug, Clone)]
pub struct Remotes {
    pub origin: Option<RemoteInfo>,
    pub upstream: Option<RemoteInfo>,
}

impl Remotes {
    pub fn resolve(repo: &Repository) -> Self {
        let parse_remote = |name: &str| repo.remote_url(name).and_then(|u| parse_url(&u));
        Self {
            origin: parse_remote("origin"),
            upstream: parse_remote("upstream"),
        }
    }

    /// The repo an MR merges into: upstream when we forked, otherwise origin.
    pub fn target(&self) -> Option<&RemoteInfo> {
        self.upstream.as_ref().or(self.origin.as_ref())
    }

    /// The owner of the branch we push, needed to express a cross-fork MR head
    /// as `owner:branch`. `None` when origin couldn't be parsed.
    pub fn head_owner(&self) -> Option<&str> {
        self.origin.as_ref().map(|r| r.owner.as_str())
    }

    /// Whether the MR crosses a fork boundary (origin and target differ in
    /// owner), which is what decides the `owner:branch` head form.
    pub fn is_cross_fork(&self) -> bool {
        match (self.origin.as_ref(), self.target()) {
            (Some(o), Some(t)) => o.owner != t.owner || o.host != t.host,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scp_syntax() {
        let info = parse_url("git@github.com:octocat/Hello-World.git").unwrap();
        assert_eq!(info.host, "github.com");
        assert_eq!(info.owner, "octocat");
        assert_eq!(info.repo, "Hello-World");
        assert_eq!(info.service, Service::GitHub);
        assert_eq!(info.project_path(), "octocat/Hello-World");
    }

    #[test]
    fn parses_https_uri_and_strips_git_suffix() {
        let info = parse_url("https://gitlab.com/group/sub/proj.git").unwrap();
        assert_eq!(info.host, "gitlab.com");
        // Nested groups are part of the owner; only the last segment is the repo.
        assert_eq!(info.owner, "group/sub");
        assert_eq!(info.repo, "proj");
        assert_eq!(info.service, Service::GitLab);
    }

    #[test]
    fn parses_ssh_uri_with_port() {
        let info = parse_url("ssh://git@codeberg.org:22/me/tool").unwrap();
        assert_eq!(info.host, "codeberg.org");
        assert_eq!(info.owner, "me");
        assert_eq!(info.repo, "tool");
        assert_eq!(info.service, Service::Codeberg);
    }

    #[test]
    fn detects_self_hosted_by_hostname_hint() {
        let info = parse_url("https://gitlab.example.com/team/app.git").unwrap();
        assert_eq!(info.service, Service::GitLab);
    }

    #[test]
    fn forgejo_is_its_own_identity() {
        // A self-hosted host naming Forgejo is detected as Forgejo, not folded
        // into Gitea — they are parallel identities now, even sharing one impl.
        let info = parse_url("https://forgejo.example.com/team/app.git").unwrap();
        assert_eq!(info.service, Service::Forgejo);
        assert_eq!(Service::parse("forgejo"), Some(Service::Forgejo));
    }

    #[test]
    fn rejects_urls_without_owner_repo() {
        assert!(parse_url("").is_none());
        assert!(parse_url("git@github.com:justrepo").is_none());
    }
}
