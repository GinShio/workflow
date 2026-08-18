//! Gitea / Forgejo / Codeberg merge requests.
//!
//! The API is GitHub-shaped but not identical, and the two differences are the
//! ones that bite: a personal token authenticates with the `token` scheme (not
//! bearer), and there is no draft *field* — a draft is signalled by a `WIP:`
//! title prefix. Listing-then-filtering replaces GitHub's head query because the
//! list endpoint is the dependable one across Gitea versions.

use serde_json::{json, Value};

use std::collections::HashMap;

use super::RemoteInfo;
use super::{
    current_user, pick, request, resolve_self, Attributes, Auth, Forge, MergeRequest, MrState,
    NewMr, StateFilter, SELF_REF,
};

const WIP_PREFIX: &str = "WIP: ";

pub struct Gitea {
    api_base: String,
    project: String,
    head_owner: Option<String>,
    auth: Auth,
}

impl Gitea {
    pub fn new(
        target: RemoteInfo,
        head_owner: Option<String>,
        token: String,
        api_url_override: Option<String>,
    ) -> Self {
        let api_base =
            api_url_override.unwrap_or_else(|| format!("https://{}/api/v1", target.host));
        Self {
            api_base,
            project: target.project_path(),
            head_owner,
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

    /// Gitea attaches labels by numeric id, so names have to be looked up against
    /// the repo's label set first.
    /// The branch's candidate PRs, one fetch, all states. Gitea's list doesn't
    /// filter by head, so we narrow to our branch here; the head's owner lives in
    /// a separate field, so matching `head.ref` against the bare branch name is
    /// correct for both same-repo and forks.
    fn candidates(&self, branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let url = format!("{}?state=all&limit=50", self.pulls_url());
        let v = request("GET", &url, &self.auth, None)?;
        Ok(v.as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|item| item["head"]["ref"].as_str() == Some(branch))
                    .filter_map(parse_pull)
                    .collect()
            })
            .unwrap_or_default())
    }

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
        source: v["head"]["ref"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["head"]["sha"].as_str().map(str::to_owned),
        body: v["body"].as_str().unwrap_or_default().to_owned(),
        web_url: v["html_url"].as_str().unwrap_or_default().to_owned(),
    })
}

impl Forge for Gitea {
    fn noun(&self) -> &'static str {
        "PR"
    }

    fn find(&self, branch: &str, state: StateFilter) -> anyhow::Result<Option<MergeRequest>> {
        Ok(pick(&self.candidates(branch)?, state))
    }

    fn find_any(&self, branch: &str) -> anyhow::Result<Option<MergeRequest>> {
        Ok(super::pick_any(&self.candidates(branch)?))
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
