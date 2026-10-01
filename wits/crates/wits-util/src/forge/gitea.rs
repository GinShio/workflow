//! Gitea / Forgejo / Codeberg merge requests.
//!
//! The API is GitHub-shaped but not identical, and the two differences are the
//! ones that bite: a personal token authenticates with the `token` scheme (not
//! bearer), and there is no draft *field* — a draft is signalled by a `WIP:`
//! title prefix. Listing-then-filtering replaces GitHub's head query because the
//! list endpoint is the dependable one across Gitea versions.

use serde_json::{json, Value};

use std::collections::HashMap;
use std::sync::OnceLock;

use super::RemoteInfo;
use super::{
    current_user, dedup_mrs, encode, next_link, request, request_every_page, resolve_self,
    Attributes, Auth, Forge, HeadRepo, MergeRequest, MrState, NewMr, SELF_REF,
};

const WIP_PREFIX: &str = "WIP: ";

pub struct Gitea {
    api_base: String,
    project: String,
    head_owner: Option<String>,
    /// The repository a stack's branches are pushed to when it is not the
    /// target; `None` when they live in the target itself.
    push_repo: Option<RemoteInfo>,
    /// `push_repo`'s numeric id, resolved by the first lookup that needs it.
    push_repo_id: OnceLock<i64>,
    auth: Auth,
}

impl Gitea {
    pub fn new(
        target: RemoteInfo,
        head_owner: Option<String>,
        push_repo: Option<RemoteInfo>,
        token: String,
        api_url_override: Option<String>,
    ) -> Self {
        let api_base =
            api_url_override.unwrap_or_else(|| format!("https://{}/api/v1", target.host));
        Self {
            api_base,
            project: target.project_path(),
            head_owner,
            push_repo,
            push_repo_id: OnceLock::new(),
            auth: Auth::Token(token),
        }
    }

    fn head_ref(&self, branch: &str) -> String {
        match &self.head_owner {
            Some(owner) => format!("{owner}:{branch}"),
            None => branch.to_owned(),
        }
    }

    fn pulls_url(&self) -> String {
        format!("{}/repos/{}/pulls", self.api_base, self.project)
    }

    fn repo_url(&self) -> String {
        format!("{}/repos/{}", self.api_base, self.project)
    }

    fn resolve_users(&self, items: &[String]) -> anyhow::Result<Vec<String>> {
        if items.iter().any(|i| i == SELF_REF) {
            let me = current_user(&self.api_base, &self.auth, "login")?;
            Ok(resolve_self(items, &me))
        } else {
            Ok(items.to_vec())
        }
    }

    /// The repository id a lookup pins `head`'s branches to: the push
    /// repository's, resolved once, or `None` when the head lives in the target.
    fn head_repo_id(&self, head: HeadRepo) -> anyhow::Result<Option<i64>> {
        let Some(push) = self.push_repo.as_ref().filter(|_| head == HeadRepo::Origin) else {
            return Ok(None);
        };
        if let Some(id) = self.push_repo_id.get() {
            return Ok(Some(*id));
        }
        let path = push.project_path();
        let v = request(
            "GET",
            &format!("{}/repos/{path}", self.api_base),
            &self.auth,
            None,
        )?;
        let id = v["id"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("could not read the id of {path}"))?;
        // Lookups run in parallel, so another one may have stored the same id first.
        Ok(Some(*self.push_repo_id.get_or_init(|| id)))
    }

    /// Gitea attaches labels by numeric id, so names have to be looked up against
    /// the repo's label set first.
    fn label_ids(&self, names: &[String]) -> anyhow::Result<Vec<i64>> {
        let v = request(
            "GET",
            &format!("{}/labels?limit=100", self.repo_url()),
            &self.auth,
            None,
        )?;
        let by_name: HashMap<&str, i64> = v
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| Some((l["name"].as_str()?, l["id"].as_i64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut ids = Vec::new();
        for name in names {
            match by_name.get(name.as_str()) {
                Some(id) => ids.push(*id),
                None => log::warn!("label '{name}' does not exist in this repo"),
            }
        }
        Ok(ids)
    }
}

/// The next page of a list that started at `base`. Only the `page` number is
/// taken from the server's `Link` header, so every request stays on the URL
/// this lookup built — the host and scheme the token is meant for — whatever
/// root URL the server builds its links from.
fn next_page(base: &str, headers: &[(String, String)]) -> Option<String> {
    let next = next_link(headers)?;
    let (_, query) = next.split_once('?')?;
    let page = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("page="))?;
    Some(format!("{base}&page={page}"))
}

/// Whether a PR's head is `branch` in the expected repository: the one with id
/// `repo` when given, else the PR's own base repository.
///
/// The branch is read from `head.label`, never `head.ref`: once the head branch
/// or its repository is deleted, `ref` turns into `refs/pull/<n>/head` while
/// `label` keeps the branch name (`services/convert/pull.go`, in Gitea and
/// Forgejo alike), and a PR merged with its branch deleted must still be found.
/// A deleted head repository reports `repo_id` -1, and a `null` element, which
/// Forgejo's list can carry, has no fields at all; neither ever matches.
fn headed_by(pr: &Value, branch: &str, repo: Option<i64>) -> bool {
    if pr["head"]["label"].as_str() != Some(branch) {
        return false;
    }
    let head = pr["head"]["repo_id"].as_i64();
    match repo {
        Some(id) => head == Some(id),
        None => head.is_some() && head == pr["base"]["repo_id"].as_i64(),
    }
}

fn parse_pull(v: &Value) -> Option<MergeRequest> {
    let number = v["number"].as_u64()?;
    let merged = v["merged"].as_bool().unwrap_or(false);
    let state = match v["state"].as_str().unwrap_or("open") {
        _ if merged => MrState::Merged,
        "closed" => MrState::Closed,
        _ => MrState::Open,
    };
    Some(MergeRequest {
        id: number.to_string(),
        display: format!("#{number}"),
        state,
        base: v["base"]["ref"].as_str().unwrap_or_default().to_owned(),
        // `label`, not `ref`, for the reason `headed_by` gives.
        source: v["head"]["label"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["head"]["sha"].as_str().map(str::to_owned),
        body: v["body"].as_str().unwrap_or_default().to_owned(),
        web_url: v["html_url"].as_str().unwrap_or_default().to_owned(),
    })
}

impl Forge for Gitea {
    fn noun(&self) -> &'static str {
        "PR"
    }

    fn mrs_for_branch(&self, head: HeadRepo, branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let head_repo = self.head_repo_id(head)?;
        // `head` filters on the server only from Forgejo 16 on (Codeberg
        // included); Gitea and older Forgejo ignore it and list every PR. The
        // check below narrows either answer to the same PRs, so only the cost
        // differs: on those servers this reads the repository's whole PR list.
        let url = format!(
            "{}?state=all&head={}&sort=recentupdate&limit=50",
            self.pulls_url(),
            encode(branch),
        );
        request_every_page(&url, &self.auth, |v, headers| {
            let prs = v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|pr| headed_by(pr, branch, head_repo))
                        .filter_map(parse_pull)
                        .collect()
                })
                .unwrap_or_default();
            (prs, next_page(&url, headers))
        })
        .map(dedup_mrs)
    }

    fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest> {
        let title = if req.draft {
            format!("{WIP_PREFIX}{}", req.title)
        } else {
            req.title.clone()
        };
        let body = json!({
            "title": title,
            "head": self.head_ref(&req.branch),
            "base": req.base,
            "body": req.body,
        });
        let v = request("POST", &self.pulls_url(), &self.auth, Some(&body))?;
        parse_pull(&v).ok_or_else(|| anyhow::anyhow!("unexpected create response: {v}"))
    }

    fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.pulls_url(), id);
        request("PATCH", &url, &self.auth, Some(&json!({ "base": base })))?;
        Ok(())
    }

    fn set_body(&self, id: &str, body: &str) -> anyhow::Result<()> {
        let url = format!("{}/{}", self.pulls_url(), id);
        request("PATCH", &url, &self.auth, Some(&json!({ "body": body })))?;
        Ok(())
    }

    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> anyhow::Result<()> {
        let issue = format!("{}/issues/{}", self.repo_url(), id);

        if !attrs.labels.is_empty() {
            let ids = self.label_ids(&attrs.labels)?;
            if !ids.is_empty() {
                // POST adds to the issue's labels without replacing them.
                let body = json!({ "labels": ids });
                if let Err(e) = request("POST", &format!("{issue}/labels"), &self.auth, Some(&body))
                {
                    log::warn!("labels: {e}");
                }
            }
        }
        if !attrs.assignees.is_empty() {
            // Gitea's issue edit replaces assignees, so union with the current set
            // to keep this additive.
            let wanted = self.resolve_users(&attrs.assignees)?;
            let mut union = current_logins(&request("GET", &issue, &self.auth, None)?);
            for name in wanted {
                if !union.contains(&name) {
                    union.push(name);
                }
            }
            let body = json!({ "assignees": union });
            if let Err(e) = request("PATCH", &issue, &self.auth, Some(&body)) {
                log::warn!("assignees: {e}");
            }
        }
        if !attrs.reviewers.is_empty() {
            let body = json!({ "reviewers": self.resolve_users(&attrs.reviewers)? });
            let url = format!("{}/{}/requested_reviewers", self.pulls_url(), id);
            if let Err(e) = request("POST", &url, &self.auth, Some(&body)) {
                log::warn!("reviewers: {e}");
            }
        }
        Ok(())
    }
}

/// The `login` of each assignee currently on an issue/PR JSON object.
fn current_logins(issue: &Value) -> Vec<String> {
    issue["assignees"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|u| u["login"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(label: &str, head_repo: i64, base_repo: i64) -> Value {
        json!({
            "number": 3, "state": "closed", "merged": true, "body": "", "html_url": "u",
            "head": { "label": label, "ref": "refs/pull/3/head", "repo_id": head_repo, "sha": "abc" },
            "base": { "ref": "main", "repo_id": base_repo },
        })
    }

    #[test]
    fn a_head_is_its_label_in_the_expected_repository() {
        // Same repository: found although the branch is gone and `ref` has
        // turned into refs/pull/3/head.
        assert!(headed_by(&pr("feat", 1, 1), "feat", None));
        // Another fork's same-named branch, or another branch, does not count.
        assert!(!headed_by(&pr("feat", 2, 1), "feat", None));
        assert!(!headed_by(&pr("feat-2", 1, 1), "feat", None));
        // A fork counts by its repository id; a deleted one (-1) never does.
        assert!(headed_by(&pr("feat", 2, 1), "feat", Some(2)));
        assert!(!headed_by(&pr("feat", -1, 1), "feat", Some(2)));
        assert!(!headed_by(&pr("feat", -1, 1), "feat", None));
        // Forgejo's list can hold `null` elements.
        assert!(!headed_by(&Value::Null, "feat", None));
    }

    #[test]
    fn a_next_page_stays_on_the_lookup_url() {
        // However the server spells its own root URL, only the page number is
        // taken from its `Link`.
        let headers = vec![(
            "link".to_owned(),
            "<http://elsewhere/api/v1/repos/o/r/pulls?head=feat&limit=50&page=2&state=all>; \
             rel=\"next\""
                .to_owned(),
        )];
        let base = "https://git.example/api/v1/repos/o/r/pulls?state=all&head=feat&limit=50";
        assert_eq!(
            next_page(base, &headers).as_deref(),
            Some("https://git.example/api/v1/repos/o/r/pulls?state=all&head=feat&limit=50&page=2")
        );
        assert_eq!(next_page(base, &[]), None);
    }

    #[test]
    fn the_source_branch_outlives_its_deletion() {
        let mr = parse_pull(&pr("feat", 1, 1)).unwrap();
        assert_eq!(mr.source, "feat");
        assert_eq!(mr.state, MrState::Merged);
    }
}
