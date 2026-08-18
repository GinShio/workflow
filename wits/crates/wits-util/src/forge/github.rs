//! GitHub (and GitHub Enterprise) merge requests — "pull requests" in its words.
//!
//! This module is pure mapping: GitHub's REST shapes in, normalized
//! [`MergeRequest`]s out. The only judgement calls are the API base (the public
//! host has a dedicated `api.github.com`, Enterprise serves under `/api/v3`) and
//! the cross-fork head form (`owner:branch`).

use std::collections::HashMap;

use serde_json::{json, Value};

use super::RemoteInfo;
use super::{
    pick, request, ActionKey, Anchor, Attributes, Auth, BatchAction, BatchOutcome, DiffVersion,
    FeedQuery, Forge, LineRef, MergeRequest, MrDetails, MrState, MrSummary, NewMr, RemoteComment,
    RemoteThread, ReviewBatch, Side, StateFilter, Verdict, SELF_REF,
};

pub struct GitHub {
    /// The GraphQL endpoint (`…/graphql`) — the whole GitHub forge speaks
    /// GraphQL (threads, resolution, PR search, and the stack half all live
    /// there); REST is used only for the object-fetch refspec, a git operation.
    graphql_url: String,
    /// The web root of the target repo (`https://host/owner/repo`), for blob
    /// permalinks — distinct from `api_base`.
    web_base: String,
    project: String,
    /// The repo we target — also the head owner for a same-repo MR.
    owner: String,
    /// The repo name alone (the GraphQL `name:` argument).
    repo: String,
    /// Set only when the head lives in a different fork.
    head_owner: Option<String>,
    auth: Auth,
}

impl GitHub {
    pub fn new(
        target: RemoteInfo,
        head_owner: Option<String>,
        token: String,
        api_url_override: Option<String>,
    ) -> Self {
        let is_dotcom = matches!(target.host.as_str(), "github.com" | "www.github.com");
        // GitHub Enterprise serves GraphQL at `https://<host>/api/graphql`; the
        // public host at `https://api.github.com/graphql`. A custom REST
        // `api-url` override (e.g. `https://host/api/v3`) is mapped to its
        // GraphQL sibling.
        let graphql_url = if is_dotcom {
            "https://api.github.com/graphql".to_owned()
        } else {
            match api_url_override {
                Some(base) => {
                    format!(
                        "{}/graphql",
                        base.trim_end_matches("/v3").trim_end_matches('/')
                    )
                }
                None => format!("https://{}/api/graphql", target.host),
            }
        };
        let web_base = format!("https://{}/{}", target.host, target.project_path());
        Self {
            graphql_url,
            web_base,
            owner: target.owner.clone(),
            repo: target.repo.clone(),
            project: target.project_path(),
            head_owner,
            auth: Auth::Bearer(token),
        }
    }

    /// POST a GraphQL query/mutation and return its `data`, surfacing any
    /// `errors[]` (GraphQL replies `200 OK` with a partial `data` + `errors`, so
    /// the HTTP status alone is not enough). Bodies and ids ride as `variables`,
    /// never string-interpolated, so arbitrary comment text is safe.
    fn graphql(&self, query: &str, variables: Value) -> anyhow::Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let v = request("POST", &self.graphql_url, &self.auth, Some(&body))?;
        if let Some(errs) = v["errors"].as_array() {
            if !errs.is_empty() {
                let msg = errs
                    .iter()
                    .filter_map(|e| e["message"].as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                anyhow::bail!("GraphQL error: {msg}");
            }
        }
        Ok(v["data"].clone())
    }

    /// Parse an MR number from its string id, for the GraphQL `number:` argument.
    fn number(&self, id: &str) -> anyhow::Result<u64> {
        id.parse::<u64>()
            .map_err(|_| anyhow::anyhow!("MR id '{id}' is not a number"))
    }

    /// The head reference for *creating*: `owner:branch` across a fork, plain
    /// `branch` within the same repo.
    fn head_ref(&self, branch: &str) -> String {
        match &self.head_owner {
            Some(owner) => format!("{owner}:{branch}"),
            None => branch.to_owned(),
        }
    }

    /// The target repository's node id (needed by `createPullRequest`).
    fn repo_node_id(&self) -> anyhow::Result<String> {
        let data = self.graphql(
            "query($owner:String!,$repo:String!){repository(owner:$owner,name:$repo){id}}",
            json!({ "owner": self.owner, "repo": self.repo }),
        )?;
        data["repository"]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("could not resolve repository node id"))
    }

    /// A pull request's node id (needed by update / label / assignee / reviewer
    /// mutations, which address the PR by node id rather than number).
    fn pr_node_id(&self, id: &str) -> anyhow::Result<String> {
        let number = self.number(id)?;
        let data = self.graphql(
            gql::ID_QUERY,
            json!({ "owner": self.owner, "repo": self.repo, "number": number }),
        )?;
        data["repository"]["pullRequest"]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("could not resolve PR node id for {id}"))
    }

    /// The viewer's pending (unpublished) review on this PR, if any — GitHub
    /// allows at most one, so this uniquely identifies an orphan left by a failed
    /// submit, for deferred cleanup.
    fn viewer_pending_review(&self, number: u64) -> anyhow::Result<Option<String>> {
        let data = self.graphql(
            gql::VIEWER_PENDING,
            json!({ "owner": self.owner, "repo": self.repo, "number": number }),
        )?;
        Ok(data["repository"]["pullRequest"]["reviews"]["nodes"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|r| r["viewerDidAuthor"].as_bool().unwrap_or(false))
                    .and_then(|r| r["id"].as_str().map(str::to_owned))
            }))
    }

    /// Resolve a user login (or `@me`) to a GraphQL user node id.
    fn user_node_id(&self, login: &str) -> anyhow::Result<Option<String>> {
        if login == SELF_REF {
            let data = self.graphql("query{viewer{id}}", json!({}))?;
            return Ok(data["viewer"]["id"].as_str().map(str::to_owned));
        }
        let data = self.graphql(
            "query($login:String!){user(login:$login){id}}",
            json!({ "login": login }),
        )?;
        Ok(data["user"]["id"].as_str().map(str::to_owned))
    }

    /// Resolve a list of user logins (with `@me`) to node ids, warning and
    /// skipping any that don't resolve — mirrors the additive, best-effort
    /// behaviour of the old REST path.
    fn resolve_user_ids(&self, logins: &[String]) -> Vec<String> {
        let mut ids = Vec::new();
        for login in logins {
            match self.user_node_id(login) {
                Ok(Some(id)) => ids.push(id),
                Ok(None) => log::warn!("user '{login}' not found"),
                Err(e) => log::warn!("resolving user '{login}': {e}"),
            }
        }
        ids
    }

    /// Resolve label names to their node ids on the target repo (one query;
    /// unknown labels warn and are skipped).
    fn label_node_ids(&self, names: &[String]) -> anyhow::Result<Vec<String>> {
        let data = self.graphql(
            "query($owner:String!,$repo:String!){repository(owner:$owner,name:$repo){labels(first:100){nodes{id name}}}}",
            json!({ "owner": self.owner, "repo": self.repo }),
        )?;
        let by_name: HashMap<&str, &str> = data["repository"]["labels"]["nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|n| Some((n["name"].as_str()?, n["id"].as_str()?)))
                    .collect()
            })
            .unwrap_or_default();
        let mut ids = Vec::new();
        for name in names {
            match by_name.get(name.as_str()) {
                Some(id) => ids.push((*id).to_owned()),
                None => log::warn!("label '{name}' not found on the repo"),
            }
        }
        Ok(ids)
    }

    /// Build a GitHub search query string from a feed's filter. `@me` in a user
    /// qualifier is passed through — GitHub resolves it server-side.
    ///
    /// One deviation from the tool's within-field-OR model: GitHub search has no
    /// label-OR qualifier, so multiple labels are AND-ed here (each `label:` is a
    /// separate, conjunctive term). GitLab honours OR. This is noted in the
    /// review docs.
    fn search_query(&self, q: &FeedQuery) -> String {
        let mut parts = vec![format!("repo:{}", self.project), "is:pr".to_owned()];
        // merged/closed are never fetched; drafts are open PRs flagged draft.
        parts.push("is:open".to_owned());
        match (q.states.open, q.states.draft) {
            (true, false) => parts.push("draft:false".to_owned()),
            (false, true) => parts.push("draft:true".to_owned()),
            _ => {}
        }
        for l in &q.labels {
            parts.push(format!("label:{}", search_quote(l)));
        }
        for l in &q.exclude_labels {
            parts.push(format!("-label:{}", search_quote(l)));
        }
        if let Some(a) = &q.author {
            parts.push(format!("author:{a}"));
        }
        if let Some(a) = &q.assignee {
            parts.push(format!("assignee:{a}"));
        }
        if let Some(r) = &q.reviewer {
            parts.push(format!("review-requested:{r}"));
        }
        if let Some(s) = &q.search {
            parts.push(s.clone());
        }
        parts.join(" ")
    }

    /// The branch's candidate PRs, one fetch, all states. GraphQL has no
    /// `head=owner:branch` filter, so we match on the head branch name and
    /// disambiguate a fork client-side by `headRepositoryOwner.login`.
    fn candidates(&self, branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let data = self.graphql(
            gql::FIND_QUERY,
            json!({ "owner": self.owner, "repo": self.repo, "branch": branch }),
        )?;
        let want = self.head_owner.as_deref().unwrap_or(&self.owner);
        Ok(data["repository"]["pullRequests"]["nodes"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|n| n["headRepositoryOwner"]["login"].as_str() == Some(want))
                    .filter_map(parse_pr_mr)
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Parse a GraphQL `PullRequest` node into an [`MrSummary`]. Shared by the feed
/// `search` and the per-MR detail query — both select the same fields, so a
/// feed-fetched PR carries `base`/`head`/`headRefOid` (unlike the old REST
/// `search/issues`, which returned issue shells).
fn parse_pr_node(v: &Value) -> Option<MrSummary> {
    let number = v["number"].as_u64()?;
    let state = match v["state"].as_str().unwrap_or("OPEN") {
        "MERGED" => MrState::Merged,
        "CLOSED" => MrState::Closed,
        _ => MrState::Open,
    };
    let labels = v["labels"]["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(MrSummary {
        id: number.to_string(),
        display: format!("#{number}"),
        state,
        draft: v["isDraft"].as_bool().unwrap_or(false),
        title: v["title"].as_str().unwrap_or_default().to_owned(),
        author: v["author"]["login"].as_str().unwrap_or_default().to_owned(),
        base: v["baseRefName"].as_str().unwrap_or_default().to_owned(),
        source: v["headRefName"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["headRefOid"].as_str().map(str::to_owned),
        updated_at: v["updatedAt"].as_str().unwrap_or_default().to_owned(),
        labels,
        web_url: v["url"].as_str().unwrap_or_default().to_owned(),
    })
}

/// Parse a GraphQL review/issue comment node.
fn parse_gql_comment(c: &Value) -> RemoteComment {
    RemoteComment {
        id: c["databaseId"].as_u64().unwrap_or(0).to_string(),
        author: c["author"]["login"].as_str().unwrap_or_default().to_owned(),
        body: c["body"].as_str().unwrap_or_default().to_owned(),
        created_at: c["createdAt"].as_str().unwrap_or_default().to_owned(),
    }
}

/// Parse a GraphQL `PullRequestReviewThread` node into a [`RemoteThread`].
/// `isResolved`/`isOutdated` come straight from the forge; the thread `id` is the
/// `PRRT_…` node id used to reply/resolve.
fn parse_review_thread(n: &Value) -> RemoteThread {
    let path = n["path"].as_str().unwrap_or_default().to_owned();
    let side = |v: &Value| match v.as_str() {
        Some("LEFT") => Side::Old,
        _ => Side::New,
    };
    // A thread's `line`/`startLine` go `null` once it is outdated (the line no
    // longer exists at the latest head); the `original*` pair is the line at the
    // commit the comment was written on. Prefer the original — it pairs with
    // `originalCommit` (the thread's anchor `commit`), so an outdated thread keeps
    // a real line instead of 0.
    let line_of =
        |primary: &str, fallback: &str| n[primary].as_u64().or_else(|| n[fallback].as_u64());
    let anchor = if n["subjectType"].as_str() == Some("FILE") {
        Some(Anchor::File { path })
    } else {
        let end = LineRef {
            line: line_of("originalLine", "line").unwrap_or(0) as u32,
            side: side(&n["diffSide"]),
        };
        let start = line_of("originalStartLine", "startLine").map(|sl| LineRef {
            line: sl as u32,
            side: side(&n["startDiffSide"]),
        });
        Some(Anchor::Line {
            path,
            old_path: None,
            end,
            start,
        })
    };
    let commit = n["comments"]["nodes"]
        .get(0)
        .and_then(|c| c["originalCommit"]["oid"].as_str())
        .map(str::to_owned);
    let comments = n["comments"]["nodes"]
        .as_array()
        .map(|a| a.iter().map(parse_gql_comment).collect())
        .unwrap_or_default();
    RemoteThread {
        id: n["id"].as_str().unwrap_or_default().to_owned(),
        resolved: n["isResolved"].as_bool().unwrap_or(false),
        outdated: n["isOutdated"].as_bool().unwrap_or(false),
        anchor,
        commit,
        comments,
    }
}

/// The GraphQL documents the review half uses. Bodies/ids ride as variables.
mod gql {
    pub const PR_FIELDS: &str = "number title url state isDraft updatedAt \
        author{login} baseRefName headRefName headRefOid \
        labels(first:50){nodes{name}}";

    pub fn details_query() -> String {
        format!(
            "query($owner:String!,$repo:String!,$number:Int!){{\
               repository(owner:$owner,name:$repo){{\
                 pullRequest(number:$number){{ {PR_FIELDS} baseRefOid }}}}}}"
        )
    }

    pub fn search_query() -> String {
        format!(
            "query($q:String!,$first:Int!,$after:String){{\
               search(query:$q,type:ISSUE,first:$first,after:$after){{\
                 pageInfo{{hasNextPage endCursor}}\
                 nodes{{... on PullRequest{{ {PR_FIELDS} }}}}}}}}"
        )
    }

    pub const ID_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!){\
        repository(owner:$owner,name:$repo){pullRequest(number:$number){id}}}";

    pub const THREADS_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!,$after:String){\
        repository(owner:$owner,name:$repo){pullRequest(number:$number){\
          reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor}\
            nodes{id isResolved isOutdated path line originalLine startLine originalStartLine \
              diffSide startDiffSide subjectType \
              comments(first:100){nodes{databaseId author{login} body createdAt originalCommit{oid}}}}}\
          reviews(first:100){nodes{databaseId author{login} body state createdAt}}\
          comments(first:100){nodes{databaseId author{login} body createdAt}}}}}";

    pub const ADD_REVIEW: &str = "mutation($input:AddPullRequestReviewInput!){\
        addPullRequestReview(input:$input){pullRequestReview{id}}}";
    pub const ADD_THREAD: &str = "mutation($input:AddPullRequestReviewThreadInput!){\
        addPullRequestReviewThread(input:$input){thread{id}}}";
    pub const SUBMIT_REVIEW: &str = "mutation($input:SubmitPullRequestReviewInput!){\
        submitPullRequestReview(input:$input){pullRequestReview{id state}}}";
    pub const ADD_COMMENT: &str = "mutation($input:AddCommentInput!){\
        addComment(input:$input){clientMutationId}}";
    pub const ADD_REPLY: &str = "mutation($input:AddPullRequestReviewThreadReplyInput!){\
        addPullRequestReviewThreadReply(input:$input){comment{id}}}";
    pub const DELETE_REVIEW: &str = "mutation($id:ID!){\
        deletePullRequestReview(input:{pullRequestReviewId:$id}){clientMutationId}}";
    pub const VIEWER_PENDING: &str = "query($owner:String!,$repo:String!,$number:Int!){\
        repository(owner:$owner,name:$repo){pullRequest(number:$number){\
          reviews(first:20,states:[PENDING]){nodes{id viewerDidAuthor}}}}}";
    pub const RESOLVE: &str =
        "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id isResolved}}}";
    pub const UNRESOLVE: &str =
        "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{id isResolved}}}";

    // --- Stack half (find / create / retarget / decorate). ---
    pub const FIND_QUERY: &str = "query($owner:String!,$repo:String!,$branch:String!){\
        repository(owner:$owner,name:$repo){pullRequests(headRefName:$branch,\
          states:[OPEN,MERGED,CLOSED],first:50,orderBy:{field:UPDATED_AT,direction:DESC}){\
          nodes{number state url baseRefName headRefName body headRefOid headRepositoryOwner{login}}}}}";
    pub const CHILDREN_QUERY: &str = "query($owner:String!,$repo:String!,$base:String!){\
        repository(owner:$owner,name:$repo){pullRequests(baseRefName:$base,\
          states:[OPEN],first:50,orderBy:{field:UPDATED_AT,direction:DESC}){\
          nodes{number state url baseRefName headRefName body headRefOid headRepositoryOwner{login}}}}}";
    pub const CREATE_PR: &str = "mutation($input:CreatePullRequestInput!){\
        createPullRequest(input:$input){pullRequest{number state url baseRefName headRefName body headRefOid}}}";
    pub const UPDATE_PR: &str = "mutation($input:UpdatePullRequestInput!){\
        updatePullRequest(input:$input){pullRequest{number}}}";
    pub const ADD_LABELS: &str = "mutation($input:AddLabelsToLabelableInput!){\
        addLabelsToLabelable(input:$input){clientMutationId}}";
    pub const ADD_ASSIGNEES: &str = "mutation($input:AddAssigneesToAssignableInput!){\
        addAssigneesToAssignable(input:$input){clientMutationId}}";
    pub const REQUEST_REVIEWS: &str = "mutation($input:RequestReviewsInput!){\
        requestReviews(input:$input){clientMutationId}}";
}

/// The `event` string GitHub's review API expects for each verdict.
fn verdict_event(v: Verdict) -> &'static str {
    match v {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    }
}

/// GitHub spells the diff sides `LEFT` (pre-image) and `RIGHT` (post-image).
fn gh_side(side: Side) -> &'static str {
    match side {
        Side::Old => "LEFT",
        Side::New => "RIGHT",
    }
}

/// Quote a search term when it contains whitespace, so `label:needs review`
/// reaches GitHub as `label:"needs review"` rather than two qualifiers.
fn search_quote(term: &str) -> String {
    if term.contains(char::is_whitespace) {
        format!("\"{term}\"")
    } else {
        term.to_owned()
    }
}

/// Parse a GraphQL `PullRequest` node into the terse [`MergeRequest`] the stack
/// verbs use. GraphQL reports the state directly as `OPEN`/`MERGED`/`CLOSED`, so
/// (unlike REST) merged and closed need no `merged_at` cross-read.
fn parse_pr_mr(v: &Value) -> Option<MergeRequest> {
    let number = v["number"].as_u64()?;
    let state = match v["state"].as_str().unwrap_or("OPEN") {
        "MERGED" => MrState::Merged,
        "CLOSED" => MrState::Closed,
        _ => MrState::Open,
    };
    Some(MergeRequest {
        id: number.to_string(),
        display: format!("#{number}"),
        state,
        base: v["baseRefName"].as_str().unwrap_or_default().to_owned(),
        source: v["headRefName"].as_str().unwrap_or_default().to_owned(),
        head_sha: v["headRefOid"].as_str().map(str::to_owned),
        body: v["body"].as_str().unwrap_or_default().to_owned(),
        web_url: v["url"].as_str().unwrap_or_default().to_owned(),
    })
}

impl Forge for GitHub {
    fn noun(&self) -> &'static str {
        "PR"
    }

    fn find(&self, branch: &str, state: StateFilter) -> anyhow::Result<Option<MergeRequest>> {
        Ok(pick(&self.candidates(branch)?, state))
    }

    fn find_any(&self, branch: &str) -> anyhow::Result<Option<MergeRequest>> {
        Ok(super::pick_any(&self.candidates(branch)?))
    }

    fn find_children(&self, base_branch: &str) -> anyhow::Result<Vec<MergeRequest>> {
        let data = self.graphql(
            gql::CHILDREN_QUERY,
            json!({ "owner": self.owner, "repo": self.repo, "base": base_branch }),
        )?;
        Ok(data["repository"]["pullRequests"]["nodes"]
            .as_array()
            .map(|arr| arr.iter().filter_map(parse_pr_mr).collect())
            .unwrap_or_default())
    }

    fn create(&self, req: &NewMr) -> anyhow::Result<MergeRequest> {
        let input = json!({
            "repositoryId": self.repo_node_id()?,
            "baseRefName": req.base,
            "headRefName": self.head_ref(&req.branch),
            "title": req.title,
            "body": req.body,
            "draft": req.draft,
        });
        let data = self.graphql(gql::CREATE_PR, json!({ "input": input }))?;
        let pr = &data["createPullRequest"]["pullRequest"];
        parse_pr_mr(pr).ok_or_else(|| anyhow::anyhow!("unexpected create response: {pr}"))
    }

    fn set_base(&self, id: &str, base: &str) -> anyhow::Result<()> {
        let pr_id = self.pr_node_id(id)?;
        self.graphql(
            gql::UPDATE_PR,
            json!({ "input": { "pullRequestId": pr_id, "baseRefName": base } }),
        )?;
        Ok(())
    }

    fn set_body(&self, id: &str, body: &str) -> anyhow::Result<()> {
        let pr_id = self.pr_node_id(id)?;
        self.graphql(
            gql::UPDATE_PR,
            json!({ "input": { "pullRequestId": pr_id, "body": body } }),
        )?;
        Ok(())
    }

    fn apply_attributes(&self, id: &str, attrs: &Attributes) -> anyhow::Result<()> {
        // GraphQL addresses labels/users by node id (not name), so each kind is
        // resolved first. All three mutations *add* (reviewers with `union:true`),
        // so the update stays additive — a project's own automation is never
        // fought — and best-effort: a sub-item that fails is logged, not fatal.
        let pr_id = self.pr_node_id(id)?;

        if !attrs.labels.is_empty() {
            match self.label_node_ids(&attrs.labels) {
                Ok(ids) if !ids.is_empty() => {
                    if let Err(e) = self.graphql(
                        gql::ADD_LABELS,
                        json!({ "input": { "labelableId": pr_id, "labelIds": ids } }),
                    ) {
                        log::warn!("labels: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("labels: {e}"),
            }
        }
        if !attrs.assignees.is_empty() {
            let ids = self.resolve_user_ids(&attrs.assignees);
            if !ids.is_empty() {
                if let Err(e) = self.graphql(
                    gql::ADD_ASSIGNEES,
                    json!({ "input": { "assignableId": pr_id, "assigneeIds": ids } }),
                ) {
                    log::warn!("assignees: {e}");
                }
            }
        }
        if !attrs.reviewers.is_empty() {
            let ids = self.resolve_user_ids(&attrs.reviewers);
            if !ids.is_empty() {
                if let Err(e) = self.graphql(
                    gql::REQUEST_REVIEWS,
                    json!({ "input": { "pullRequestId": pr_id, "userIds": ids, "union": true } }),
                ) {
                    log::warn!("reviewers: {e}");
                }
            }
        }
        Ok(())
    }

    fn list_mrs(&self, query: &FeedQuery) -> anyhow::Result<Vec<MrSummary>> {
        // GraphQL `search` returns real `PullRequest` nodes (base/head included),
        // paginated by cursor. The query string reuses the same faceted builder.
        let q = self.search_query(query);
        let limit = query.limit.max(1);
        let doc = gql::search_query();
        let mut out: Vec<MrSummary> = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..10 {
            if out.len() >= limit {
                break;
            }
            let first = (limit - out.len()).clamp(1, 100) as u64;
            let data = self.graphql(&doc, json!({ "q": q, "first": first, "after": after }))?;
            let search = &data["search"];
            if let Some(nodes) = search["nodes"].as_array() {
                out.extend(nodes.iter().filter_map(parse_pr_node));
            }
            let has_next = search["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            after = search["pageInfo"]["endCursor"].as_str().map(str::to_owned);
            if !has_next || after.is_none() {
                break;
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    fn mr_details(&self, id: &str) -> anyhow::Result<MrDetails> {
        let number = self.number(id)?;
        let data = self.graphql(
            &gql::details_query(),
            json!({ "owner": self.owner, "repo": self.repo, "number": number }),
        )?;
        let pr = &data["repository"]["pullRequest"];
        let summary =
            parse_pr_node(pr).ok_or_else(|| anyhow::anyhow!("unexpected PR response: {pr}"))?;
        // GitHub anchors a review at a single commitOID (the reviewed head); it
        // has no per-comment start SHA, so start mirrors base.
        let base_sha = pr["baseRefOid"].as_str().unwrap_or_default().to_owned();
        let head_sha = pr["headRefOid"].as_str().unwrap_or_default().to_owned();
        let version = DiffVersion {
            base_sha: base_sha.clone(),
            start_sha: base_sha,
            head_sha,
        };
        Ok(MrDetails { summary, version })
    }

    fn mr_ref(&self, id: &str) -> anyhow::Result<String> {
        Ok(format!("refs/pull/{id}/head"))
    }

    fn list_threads(&self, id: &str) -> anyhow::Result<Vec<RemoteThread>> {
        // GraphQL groups review threads natively and exposes `isResolved` /
        // `isOutdated` — no `in_reply_to_id` walk, no `line == null` heuristic.
        // Conversation (issue) comments come back on the same query as MR-level
        // threads (anchor `None`).
        let number = self.number(id)?;
        let mut threads: Vec<RemoteThread> = Vec::new();
        let mut after: Option<String> = None;
        let mut first_page = true;
        for _ in 0..10 {
            let data = self.graphql(
                gql::THREADS_QUERY,
                json!({ "owner": self.owner, "repo": self.repo, "number": number, "after": after }),
            )?;
            let pr = &data["repository"]["pullRequest"];
            let rt = &pr["reviewThreads"];
            if let Some(nodes) = rt["nodes"].as_array() {
                threads.extend(nodes.iter().map(parse_review_thread));
            }
            // Conversation comments and review summary bodies aren't paginated by
            // the thread cursor; take them once, on the first page.
            if first_page {
                // A review's top-level body (the "Requested changes: …" text that
                // rides a verdict) is neither a review thread nor an issue
                // comment, so it must be read from `reviews` or it's invisible.
                // Skip PENDING (your own unsubmitted) and body-less reviews (a
                // bare verdict, or one whose remarks are all inline threads).
                if let Some(nodes) = pr["reviews"]["nodes"].as_array() {
                    for r in nodes {
                        let body = r["body"].as_str().unwrap_or_default();
                        if body.is_empty() || r["state"].as_str() == Some("PENDING") {
                            continue;
                        }
                        threads.push(RemoteThread {
                            id: r["databaseId"].as_u64().unwrap_or(0).to_string(),
                            resolved: false,
                            outdated: false,
                            anchor: None,
                            commit: None,
                            comments: vec![parse_gql_comment(r)],
                        });
                    }
                }
                if let Some(nodes) = pr["comments"]["nodes"].as_array() {
                    for c in nodes {
                        threads.push(RemoteThread {
                            id: c["databaseId"].as_u64().unwrap_or(0).to_string(),
                            resolved: false,
                            outdated: false,
                            anchor: None,
                            commit: None,
                            comments: vec![parse_gql_comment(c)],
                        });
                    }
                }
                first_page = false;
            }
            let has_next = rt["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            after = rt["pageInfo"]["endCursor"].as_str().map(str::to_owned);
            if !has_next || after.is_none() {
                break;
            }
        }
        Ok(threads)
    }

    fn submit(&self, id: &str, batch: &ReviewBatch) -> anyhow::Result<BatchOutcome> {
        let number = self.number(id)?;

        // Pre-flight (deferred cleanup): discard the pending review a *prior*
        // failed attempt left orphaned, before creating a new one — GitHub allows
        // only one pending review per PR, so a leftover would otherwise block
        // every future submit. Best-effort: if the delete fails, a truly-still-
        // pending review makes the create below fail, and we re-discover and
        // re-record it (self-healing); a review that has since published simply
        // can't be re-deleted, which is fine. We only delete ids we recorded.
        // A stale delete that *fails* is kept as still-in-flight so the next
        // attempt retries it — otherwise a cleanup-only submit (empty batch)
        // would clear the record and orphan the review forever. On the non-empty
        // path a truly-still-pending review also makes the create below fail and
        // is re-discovered, so the id is deduped at the end.
        let mut inflight: Vec<String> = Vec::new();
        for rid in &batch.stale {
            if self
                .graphql(gql::DELETE_REVIEW, json!({ "id": rid }))
                .is_err()
            {
                inflight.push(rid.clone());
            }
        }
        if batch.is_empty() {
            return Ok(BatchOutcome {
                inflight,
                ..Default::default()
            });
        }

        // Resolve the PR node id first — `Err` here means we couldn't even start,
        // so nothing landed and the caller keeps the whole draft.
        let data = self.graphql(
            gql::ID_QUERY,
            json!({ "owner": self.owner, "repo": self.repo, "number": number }),
        )?;
        let pr_id = data["repository"]["pullRequest"]["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("could not resolve PR node id for {id}"))?
            .to_owned();

        // Partition the batch by how GitHub GraphQL lands each kind.
        let mut line_threads: Vec<(ActionKey, Value)> = Vec::new();
        let mut file_comments: Vec<(ActionKey, String, String)> = Vec::new();
        let mut mr_comments: Vec<(ActionKey, String)> = Vec::new();
        let mut replies: Vec<(ActionKey, String, String)> = Vec::new();
        let mut resolves: Vec<(ActionKey, String, bool)> = Vec::new();
        for a in &batch.actions {
            match a {
                BatchAction::Comment {
                    key, anchor, body, ..
                } => match anchor {
                    Some(Anchor::Line {
                        path, end, start, ..
                    }) => {
                        let mut t = json!({
                            "path": path, "line": end.line,
                            "side": gh_side(end.side), "body": body,
                        });
                        if let Some(s) = start {
                            t["startLine"] = json!(s.line);
                            t["startSide"] = json!(gh_side(s.side));
                        }
                        line_threads.push((key.clone(), t));
                    }
                    Some(Anchor::File { path }) => {
                        file_comments.push((key.clone(), path.clone(), body.clone()))
                    }
                    None => mr_comments.push((key.clone(), body.clone())),
                },
                BatchAction::Reply { key, thread, body } => {
                    replies.push((key.clone(), thread.clone(), body.clone()))
                }
                BatchAction::Resolve {
                    key,
                    thread,
                    resolved,
                } => resolves.push((key.clone(), thread.clone(), *resolved)),
            }
        }

        let mut landed: HashMap<ActionKey, bool> = HashMap::new();
        let mut notifications = 0u32;
        let mut summary_ok = batch.summary.is_none();
        let mut verdict_ok = batch.verdict.map(|_| false);
        // `inflight` (forge-side reviews this attempt leaves unpublished) was
        // seeded above with any stale id whose pre-flight delete failed.

        // --- The review: line threads + file threads + replies + summary +
        // verdict, folded into ONE review (one notification), exactly as the web
        // UI does. File-level threads and replies can only join a *pending*
        // review (a reply carries the pending review's id — the same mechanism
        // the UI uses), so their presence switches to the create → attach →
        // submit flow; a review of only line comments/summary/verdict is a single
        // atomic `addPullRequestReview`. ---
        let event = batch.verdict.map(verdict_event).unwrap_or("COMMENT");
        let needs_pending = !file_comments.is_empty() || !replies.is_empty();
        let has_content = !line_threads.is_empty() || needs_pending || batch.summary.is_some();
        // GitHub rejects a COMMENT/REQUEST_CHANGES review carrying neither a body
        // nor a comment; only APPROVE may be empty. So attempt the review when
        // there is content, or when the verdict is a (possibly bare) approval.
        let has_review = has_content || batch.verdict == Some(Verdict::Approve);
        if !has_review {
            if let Some(v @ (Verdict::Comment | Verdict::RequestChanges)) = batch.verdict {
                log::warn!(
                    "MR {id}: a '{}' verdict needs a comment or summary on GitHub; \
                     it stays in the draft",
                    v.display_str()
                );
            }
        }
        let line_keys = || line_threads.iter().map(|(k, _)| k.clone());
        let review_keys = || {
            line_keys()
                .chain(file_comments.iter().map(|(k, ..)| k.clone()))
                .chain(replies.iter().map(|(k, ..)| k.clone()))
        };
        if has_review {
            let threads_json: Vec<Value> = line_threads.iter().map(|(_, t)| t.clone()).collect();
            let mut input = json!({
                "pullRequestId": pr_id,
                "commitOID": batch.version.head_sha,
                "threads": threads_json,
            });
            if let Some(s) = &batch.summary {
                input["body"] = json!(s);
            }

            if !needs_pending {
                // One atomic review: line threads + summary + verdict.
                input["event"] = json!(event);
                match self.graphql(gql::ADD_REVIEW, json!({ "input": input })) {
                    Ok(_) => {
                        for k in line_keys() {
                            landed.insert(k, true);
                        }
                        summary_ok = true;
                        verdict_ok = batch.verdict.map(|_| true);
                        notifications += 1;
                    }
                    Err(e) => {
                        log::warn!("MR {id}: review failed: {e}");
                        for k in line_keys() {
                            landed.insert(k, false);
                        }
                    }
                }
            } else {
                // Pending review → attach file threads and replies → submit, so
                // all of them publish inside the one review notification.
                match self.graphql(gql::ADD_REVIEW, json!({ "input": input })) {
                    Ok(d) => {
                        let review_id = d["addPullRequestReview"]["pullRequestReview"]["id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        for k in line_keys() {
                            landed.insert(k, true);
                        }
                        for (k, path, body) in &file_comments {
                            let fin = json!({
                                "pullRequestReviewId": review_id,
                                "path": path, "subjectType": "FILE", "body": body,
                            });
                            let ok = self
                                .graphql(gql::ADD_THREAD, json!({ "input": fin }))
                                .is_ok();
                            landed.insert(k.clone(), ok);
                        }
                        for (k, thread, body) in &replies {
                            let rin = json!({
                                "pullRequestReviewId": review_id,
                                "pullRequestReviewThreadId": thread,
                                "body": body,
                            });
                            let ok = self
                                .graphql(gql::ADD_REPLY, json!({ "input": rin }))
                                .is_ok();
                            landed.insert(k.clone(), ok);
                        }
                        match self.graphql(
                            gql::SUBMIT_REVIEW,
                            json!({ "input": { "pullRequestReviewId": review_id, "event": event } }),
                        ) {
                            Ok(_) => {
                                summary_ok = true;
                                verdict_ok = batch.verdict.map(|_| true);
                                notifications += 1;
                            }
                            Err(e) => {
                                log::warn!("MR {id}: submitting the pending review failed: {e}");
                                for k in review_keys() {
                                    landed.insert(k, false);
                                }
                                // The pending review + its attached comments stay
                                // on GitHub, orphaned — record it so the next
                                // attempt's pre-flight discards it before retrying.
                                inflight.push(review_id.clone());
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("MR {id}: creating the review failed: {e}");
                        for k in review_keys() {
                            landed.insert(k, false);
                        }
                        // Create can fail because a prior orphaned pending review
                        // still blocks the slot (pre-flight couldn't clear it).
                        // Re-discover it so the next attempt can delete it.
                        if let Ok(Some(rid)) = self.viewer_pending_review(number) {
                            inflight.push(rid);
                        }
                    }
                }
            }
        }

        // --- MR-level conversation comments: each a separate issue comment (its
        // own notification — a genuine API limit that a review batch can't fold
        // in, counted honestly). ---
        for (k, body) in &mr_comments {
            let ok = self
                .graphql(
                    gql::ADD_COMMENT,
                    json!({ "input": { "subjectId": pr_id, "body": body } }),
                )
                .is_ok();
            if ok {
                notifications += 1;
            } else {
                log::warn!("MR {id}: conversation comment failed");
            }
            landed.insert(k.clone(), ok);
        }
        // --- Resolves / unresolves. ---
        for (k, thread, resolved) in &resolves {
            let doc = if *resolved {
                gql::RESOLVE
            } else {
                gql::UNRESOLVE
            };
            let ok = self.graphql(doc, json!({ "id": thread })).is_ok();
            if !ok {
                log::warn!("MR {id}: resolve of {thread} failed");
            }
            landed.insert(k.clone(), ok);
        }

        // A failed stale delete and a re-discovered orphan can be the same id;
        // keep the record unique so cleanup doesn't try the same delete twice.
        inflight.sort();
        inflight.dedup();

        Ok(BatchOutcome {
            landed,
            summary_ok,
            verdict_ok,
            notifications,
            inflight,
        })
    }

    fn permalink(&self, r#ref: &str, path: &str, lines: Option<(u32, Option<u32>)>) -> String {
        let frag = match lines {
            Some((a, Some(b))) => format!("#L{a}-L{b}"),
            Some((a, None)) => format!("#L{a}"),
            None => String::new(),
        };
        format!(
            "{}/blob/{ref}/{}{frag}",
            self.web_base,
            super::encode_path(path)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::Service;
    use serde_json::json;

    fn gh() -> GitHub {
        GitHub::new(
            RemoteInfo {
                host: "github.com".into(),
                owner: "o".into(),
                repo: "r".into(),
                service: Service::GitHub,
            },
            None,
            "t".into(),
            None,
        )
    }

    #[test]
    fn parses_a_graphql_pr_node() {
        let v = json!({
            "number": 123, "state": "OPEN", "isDraft": true, "title": "T", "url": "U",
            "author": { "login": "alice" }, "baseRefName": "main", "headRefName": "feat",
            "headRefOid": "deadbeef", "updatedAt": "2026-07-01T00:00:00Z",
            "labels": { "nodes": [ { "name": "bug" } ] }
        });
        let s = parse_pr_node(&v).unwrap();
        assert_eq!(s.id, "123");
        assert_eq!(s.state, MrState::Open);
        assert!(s.draft);
        assert_eq!(s.base, "main");
        assert_eq!(s.source, "feat");
        assert_eq!(s.head_sha.as_deref(), Some("deadbeef"));
        assert_eq!(s.labels, ["bug"]);
    }

    #[test]
    fn graphql_state_maps_merged_closed_open() {
        let mk = |st: &str| {
            parse_pr_mr(
                &json!({ "number": 1, "state": st, "baseRefName": "m", "url": "u", "body": "" }),
            )
            .unwrap()
            .state
        };
        assert_eq!(mk("MERGED"), MrState::Merged);
        assert_eq!(mk("CLOSED"), MrState::Closed);
        assert_eq!(mk("OPEN"), MrState::Open);
    }

    #[test]
    fn parses_a_multi_line_review_thread() {
        let v = json!({
            "id": "PRRT_1", "isResolved": true, "isOutdated": false, "path": "src/x.c",
            "line": 42, "startLine": 40, "diffSide": "RIGHT", "startDiffSide": "RIGHT",
            "subjectType": "LINE",
            "comments": { "nodes": [ {
                "databaseId": 5, "author": { "login": "bob" }, "body": "nit",
                "createdAt": "t", "originalCommit": { "oid": "abc" }
            } ] }
        });
        let t = parse_review_thread(&v);
        assert_eq!(t.id, "PRRT_1");
        assert!(t.resolved && !t.outdated);
        assert_eq!(t.commit.as_deref(), Some("abc"));
        match t.anchor {
            Some(Anchor::Line {
                path, end, start, ..
            }) => {
                assert_eq!(path, "src/x.c");
                assert_eq!((end.line, end.side), (42, Side::New));
                assert_eq!(start.unwrap().line, 40);
            }
            other => panic!("expected a line anchor, got {other:?}"),
        }
        assert_eq!(t.comments[0].id, "5");
        assert_eq!(t.comments[0].author, "bob");
    }

    #[test]
    fn parses_a_file_review_thread() {
        let v = json!({
            "id": "PRRT_2", "isResolved": false, "isOutdated": true, "path": "a.c",
            "subjectType": "FILE", "comments": { "nodes": [] }
        });
        let t = parse_review_thread(&v);
        assert!(matches!(t.anchor, Some(Anchor::File { .. })));
        assert!(t.outdated);
    }

    #[test]
    fn search_query_builds_faceted_terms() {
        let q = FeedQuery {
            labels: vec!["bug".into()],
            author: Some("alice".into()),
            ..Default::default()
        };
        let s = gh().search_query(&q);
        for term in ["repo:o/r", "is:pr", "is:open", "label:bug", "author:alice"] {
            assert!(s.contains(term), "missing {term} in {s}");
        }
    }

    #[test]
    fn sides_and_verdict_events() {
        assert_eq!(gh_side(Side::Old), "LEFT");
        assert_eq!(gh_side(Side::New), "RIGHT");
        assert_eq!(verdict_event(Verdict::Approve), "APPROVE");
        assert_eq!(verdict_event(Verdict::RequestChanges), "REQUEST_CHANGES");
        assert_eq!(verdict_event(Verdict::Comment), "COMMENT");
    }

    #[test]
    fn permalink_encodes_path_but_keeps_slashes() {
        let url = gh().permalink("deadbeef", "src/a b.c", Some((5, None)));
        assert_eq!(url, "https://github.com/o/r/blob/deadbeef/src/a%20b.c#L5");
    }
}
