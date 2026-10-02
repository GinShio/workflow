//! Gitea / Forgejo / Codeberg merge requests.
//!
//! The API is GitHub's REST API in shape but not in detail, and the
//! differences that bite are behaviours: there is no draft *field* — a draft
//! is signalled by a `WIP:` title prefix (the server's default
//! `WORK_IN_PROGRESS_PREFIXES`) — and the PR list filters by head branch only
//! from Forgejo 16 on, so a branch's PRs are found by listing and checking
//! each one. The servers differ among themselves too, and the code says where.

use serde_json::{json, Value};

use std::sync::OnceLock;

use super::RemoteInfo;
use super::{
    dedup_mrs, encode, next_link, request, request_every_page, resolve_self, Attributes, Auth,
    Forge, HeadRepo, MergeRequest, MrComment, MrState, NewMr, SELF_REF,
};

const WIP_PREFIX: &str = "WIP: ";

pub struct Gitea {
    api_base: String,
    project: String,
    /// The target's owner, which a fork's head is told apart from.
    owner: String,
    /// The repository a stack's branches are pushed to when it is not the
    /// target; `None` when they live in the target itself.
    push_repo: Option<RemoteInfo>,
    /// `push_repo`'s numeric id, resolved by the first lookup that needs it.
    push_repo_id: OnceLock<i64>,
    /// The authenticated user's id and login, read once by the first request
    /// that needs either.
    me: OnceLock<(i64, String)>,
    auth: Auth,
}

impl Gitea {
    pub fn new(
        target: RemoteInfo,
        push_repo: Option<RemoteInfo>,
        token: String,
        api_url_override: Option<String>,
    ) -> Self {
        let api_base =
            api_url_override.unwrap_or_else(|| format!("https://{}/api/v1", target.host));
        Self {
            api_base,
            project: target.project_path(),
            owner: target.owner,
            push_repo,
            push_repo_id: OnceLock::new(),
            me: OnceLock::new(),
            auth: Auth::Token(token),
        }
    }

    /// The authenticated user's id and login, read once.
    fn me(&self) -> anyhow::Result<&(i64, String)> {
        if let Some(me) = self.me.get() {
            return Ok(me);
        }
        let v = request("GET", &format!("{}/user", self.api_base), &self.auth, None)?;
        let (Some(id), Some(login)) = (v["id"].as_i64(), v["login"].as_str()) else {
            anyhow::bail!("could not read the authenticated user");
        };
        // Requests run in parallel, so another one may have stored the same user first.
        Ok(self.me.get_or_init(|| (id, login.to_owned())))
    }

    /// The `head` a pull request is created from: the branch alone in the
    /// target, `owner:branch` from a fork. A fork the target's own owner holds
    /// needs `owner/repo:branch`, since `owner:branch` resolves to the target
    /// itself. Only Gitea 1.26 and later parse that form (go-gitea/gitea#36105);
    /// Forgejo refuses it with a 404 rather than opening the PR from the wrong
    /// repository.
    fn head_ref(&self, branch: &str) -> String {
        match &self.push_repo {
            None => branch.to_owned(),
            Some(push) if push.owner.eq_ignore_ascii_case(&self.owner) => {
                format!("{}:{branch}", push.project_path())
            }
            Some(push) => format!("{}:{branch}", push.owner),
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
            Ok(resolve_self(items, &self.me()?.1))
        } else {
            Ok(items.to_vec())
        }
    }

    /// The commit pull request `number`'s head was at when it stopped being
    /// open: its pull ref, which only an open PR's pushes move (`pull_list.go`
    /// in Gitea and Forgejo). A PR listing will not do — Forgejo's gives the
    /// head *branch*'s tip, which a reused branch name moves on.
    fn pull_head(&self, number: &str) -> anyhow::Result<Option<String>> {
        let url = format!("{}/git/refs/pull/{number}/head", self.repo_url());
        Ok(pull_ref_sha(
            &request("GET", &url, &self.auth, None)?,
            number,
        ))
    }

    /// Add `names` to the issue's assignees. The issue edit replaces the set,
    /// so it is read and unioned first to keep this additive.
    fn add_assignees(&self, issue: &str, names: &[String]) -> anyhow::Result<()> {
        let wanted = self.resolve_users(names)?;
        let mut union = field_of(
            &request("GET", issue, &self.auth, None)?["assignees"],
            "login",
        );
        for name in &wanted {
            if !union.contains(name) {
                union.push(name.clone());
            }
        }
        let body = json!({ "assignees": union });
        let updated = request("PATCH", issue, &self.auth, Some(&body))?;
        // Without write access the edit drops `assignees` and still succeeds
        // (Gitea's and Forgejo's `EditIssue`).
        for name in missing(&wanted, &field_of(&updated["assignees"], "login")) {
            log::warn!("assignee '{name}' was not added; that takes write access to the repo");
        }
        Ok(())
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
}

/// The commit `refs/pull/<number>/head` names in a `git/refs` answer, which
/// lists every ref the requested name prefixes.
fn pull_ref_sha(refs: &Value, number: &str) -> Option<String> {
    let name = format!("refs/pull/{number}/head");
    let found = refs
        .as_array()?
        .iter()
        .find(|r| r["ref"].as_str() == Some(name.as_str()))?;
    found["object"]["sha"].as_str().map(str::to_owned)
}

/// The `field` of every object in a JSON array — an empty list for `null`,
/// which is how Gitea spells an empty assignee list.
fn field_of(items: &Value, field: &str) -> Vec<String> {
    items
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The `wanted` names `got` lacks, without regard to case: Gitea and Forgejo
/// match logins that way, and a label found in another case is on the issue
/// all the same.
fn missing<'a>(wanted: &'a [String], got: &[String]) -> Vec<&'a str> {
    wanted
        .iter()
        .filter(|want| !got.iter().any(|g| g.eq_ignore_ascii_case(want)))
        .map(String::as_str)
        .collect()
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
    let join = if base.contains('?') { '&' } else { '?' };
    Some(format!("{base}{join}page={page}"))
}

/// A comment as an [`MrComment`], `own` when user `me` wrote it.
fn parse_comment(comment: &Value, me: i64) -> Option<(i64, MrComment)> {
    let id = comment["id"].as_i64()?;
    let parsed = MrComment {
        id: id.to_string(),
        body: comment["body"].as_str().unwrap_or_default().to_owned(),
        url: comment["html_url"].as_str().unwrap_or_default().to_owned(),
        own: comment["user"]["id"].as_i64() == Some(me),
    };
    Some((id, parsed))
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
        let prs = request_every_page(&url, &self.auth, |v, headers| {
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
        })?;
        let mut prs = dedup_mrs(prs);
        for pr in prs.iter_mut().filter(|pr| pr.state != MrState::Open) {
            pr.head_sha = self.pull_head(&pr.id)?;
        }
        Ok(prs)
    }

    fn list_comments(&self, mr: &str) -> anyhow::Result<Vec<MrComment>> {
        let me = self.me()?.0;
        // The endpoint takes no paging parameters and answers with every
        // comment (`issueGetComments` in Gitea's and Forgejo's API spec); a
        // `Link` header, should a server page it after all, is still followed.
        let url = format!("{}/issues/{mr}/comments", self.repo_url());
        let mut comments = request_every_page(&url, &self.auth, |v, headers| {
            let comments = v
                .as_array()
                .map(|arr| arr.iter().filter_map(|c| parse_comment(c, me)).collect())
                .unwrap_or_default();
            (comments, next_page(&url, headers))
        })?;
        // Ids grow with time, so they order the comments oldest first whatever
        // order the server lists them in.
        comments.sort_by_key(|(id, _)| *id);
        Ok(comments.into_iter().map(|(_, comment)| comment).collect())
    }

    fn add_comment(&self, mr: &str, body: &str) -> anyhow::Result<()> {
        let url = format!("{}/issues/{mr}/comments", self.repo_url());
        request("POST", &url, &self.auth, Some(&json!({ "body": body })))?;
        Ok(())
    }

    fn edit_comment(&self, _mr: &str, comment: &str, body: &str) -> anyhow::Result<()> {
        let url = format!("{}/issues/comments/{comment}", self.repo_url());
        request("PATCH", &url, &self.auth, Some(&json!({ "body": body })))?;
        Ok(())
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
            // By name, which also finds the organisation's labels (Gitea 1.23,
            // Forgejo 10) that the repository's own list leaves out; POST adds
            // to the issue's labels. A name the server does not know is dropped
            // without an error, so the labels it answers with are checked.
            let body = json!({ "labels": attrs.labels });
            match request("POST", &format!("{issue}/labels"), &self.auth, Some(&body)) {
                Ok(labels) => {
                    for name in missing(&attrs.labels, &field_of(&labels, "name")) {
                        log::warn!(
                            "label '{name}' does not exist in this repo or its organisation"
                        );
                    }
                }
                Err(e) => log::warn!("labels: {e}"),
            }
        }
        if !attrs.assignees.is_empty() {
            if let Err(e) = self.add_assignees(&issue, &attrs.assignees) {
                log::warn!("assignees: {e}");
            }
        }
        if !attrs.reviewers.is_empty() {
            // One request each: the server stops at the first reviewer it
            // refuses — the PR's own author among them — and keeps or drops the
            // ones before it depending on the server.
            let url = format!("{}/{}/requested_reviewers", self.pulls_url(), id);
            match self.resolve_users(&attrs.reviewers) {
                Ok(reviewers) => {
                    for reviewer in reviewers {
                        let body = json!({ "reviewers": [reviewer] });
                        if let Err(e) = request("POST", &url, &self.auth, Some(&body)) {
                            log::warn!("reviewer '{reviewer}': {e}");
                        }
                    }
                }
                Err(e) => log::warn!("reviewers: {e}"),
            }
        }
        Ok(())
    }
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
        // A base without a query of its own starts one.
        assert_eq!(
            next_page(
                "https://git.example/api/v1/repos/o/r/issues/3/comments",
                &headers
            )
            .as_deref(),
            Some("https://git.example/api/v1/repos/o/r/issues/3/comments?page=2")
        );
    }

    #[test]
    fn a_comment_is_marked_by_its_author_id() {
        let comment = |id: i64, user: i64| json!({ "id": id, "body": "b", "html_url": "u", "user": { "id": user, "login": "x" } });
        let (id, mine) = parse_comment(&comment(4, 9), 9).unwrap();
        assert_eq!((id, mine.id.as_str(), mine.own), (4, "4", true));
        assert!(!parse_comment(&comment(5, 8), 9).unwrap().1.own);
    }

    #[test]
    fn the_source_branch_outlives_its_deletion() {
        let mr = parse_pull(&pr("feat", 1, 1)).unwrap();
        assert_eq!(mr.source, "feat");
        assert_eq!(mr.state, MrState::Merged);
    }

    #[test]
    fn a_closed_pr_keeps_the_head_of_its_pull_ref() {
        let refs = json!([
            { "ref": "refs/pull/920/head", "object": { "type": "commit", "sha": "aaa" } },
            { "ref": "refs/pull/920/headless", "object": { "type": "commit", "sha": "bbb" } },
        ]);
        assert_eq!(pull_ref_sha(&refs, "920").as_deref(), Some("aaa"));
        assert_eq!(pull_ref_sha(&refs, "92"), None);
        assert_eq!(pull_ref_sha(&json!({ "message": "x" }), "920"), None);
    }

    #[test]
    fn what_an_update_left_out_is_found_in_its_answer() {
        let labels = json!([{ "id": 1, "name": "Bug" }, { "id": 2, "name": "infra" }]);
        let got = field_of(&labels, "name");
        let wanted = ["bug".to_owned(), "typo".to_owned()];
        assert_eq!(missing(&wanted, &got), ["typo"]);
        // Gitea answers an empty assignee list with `null`.
        assert!(field_of(&Value::Null, "login").is_empty());
    }

    #[test]
    fn a_fork_head_names_as_much_as_tells_it_apart() {
        let repo = |owner: &str, repo: &str| RemoteInfo {
            host: "codeberg.org".into(),
            owner: owner.into(),
            repo: repo.into(),
            service: super::super::Service::Codeberg,
        };
        let gitea = |push| Gitea::new(repo("org", "tool"), push, "t".into(), None);
        assert_eq!(gitea(None).head_ref("feat"), "feat");
        assert_eq!(gitea(Some(repo("me", "tool"))).head_ref("feat"), "me:feat");
        // The target owner's own fork: the owner alone would name the target.
        assert_eq!(
            gitea(Some(repo("org", "tool-next"))).head_ref("feat"),
            "org/tool-next:feat"
        );
    }
}
