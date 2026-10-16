// Links to the pages of a commit, a branch, a tag or a file on the web host of a remote, like
// lazygit's "open in browser": the remote URL becomes https://host/owner/repo, and each kind of
// host lays out its pages its own way.

use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::git::{split_remote_branch, Repository};

// What to link to, from the selected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Commit(String),
    // a local branch: a new pull request from it
    Branch(String),
    // "origin/main"
    RemoteBranch(String),
    Tag(String),
    File { commit: String, path: String },
    Remote(String),
}

// A host of `[core.browser.services]`: a kind ("gitlab"), the kind and the domain of its pages
// ("github:github.com", for an ssh alias), or its own link patterns.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Service {
    Kind(String),
    Links(Links),
}

// The patterns of the links of a host: {base} is https://host/owner/repo, and {hash}, {branch},
// {tag} and {path} what is linked to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Links {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub pull_request: Option<String>,
    pub tag: Option<String>,
    pub file: Option<String>,
}

const GITHUB: [&str; 5] = [
    "{base}/commit/{hash}",
    "{base}/tree/{branch}",
    "{base}/compare/{branch}?expand=1",
    "{base}/releases/tag/{tag}",
    "{base}/blob/{hash}/{path}",
];
const GITLAB: [&str; 5] = [
    "{base}/-/commit/{hash}",
    "{base}/-/tree/{branch}",
    "{base}/-/merge_requests/new?merge_request[source_branch]={branch}",
    "{base}/-/tags/{tag}",
    "{base}/-/blob/{hash}/{path}",
];
const BITBUCKET: [&str; 5] = [
    "{base}/commits/{hash}",
    "{base}/branch/{branch}",
    "{base}/pull-requests/new?source={branch}&t=1",
    "{base}/src/{tag}",
    "{base}/src/{hash}/{path}",
];
const GITEA: [&str; 5] = [
    "{base}/commit/{hash}",
    "{base}/src/branch/{branch}",
    "{base}/compare/{branch}",
    "{base}/src/tag/{tag}",
    "{base}/src/commit/{hash}/{path}",
];

impl Links {
    fn of_kind(kind: &str) -> Option<Links> {
        let [commit, branch, pull_request, tag, file] = match kind {
            "github" => GITHUB,
            "gitlab" => GITLAB,
            "bitbucket" => BITBUCKET,
            "gitea" | "forgejo" | "codeberg" => GITEA,
            _ => return None,
        };
        let some = |s: &str| Some(s.to_string());
        Some(Links {
            commit: some(commit),
            branch: some(branch),
            pull_request: some(pull_request),
            tag: some(tag),
            file: some(file),
        })
    }
}

// A page of the repository on its host.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Page {
    Repo,
    Commit(String),
    Branch(String),
    PullRequest(String),
    Tag(String),
    File { commit: String, path: String },
}

// The link of `target`, on the web host of the remote it belongs to.
pub fn link(
    repository: &Repository,
    services: &FxHashMap<String, Service>,
    target: &LinkTarget,
) -> Result<String, String> {
    let info = repository.remote_info();
    // the upstream remote of the branch, else origin, else the first
    let remote_of = |branch: Option<&str>| {
        branch
            .and_then(|b| info.upstreams.get(b))
            .map(|(remote, _)| remote.clone())
            .or_else(|| info.remotes.iter().find(|r| *r == "origin").cloned())
            .or_else(|| info.remotes.first().cloned())
            .ok_or_else(|| "There is no remote to link to".to_string())
    };
    let current = info.current_branch.as_deref();
    let (remote, page) = match target {
        LinkTarget::Commit(hash) => (remote_of(current)?, Page::Commit(hash.clone())),
        LinkTarget::File { commit, path } => (
            remote_of(current)?,
            Page::File {
                commit: commit.clone(),
                path: path.clone(),
            },
        ),
        LinkTarget::Tag(tag) => (remote_of(current)?, Page::Tag(tag.clone())),
        LinkTarget::Remote(remote) => (remote.clone(), Page::Repo),
        LinkTarget::RemoteBranch(name) => {
            let (remote, branch) = split_remote_branch(&info.remotes, name)
                .ok_or_else(|| format!("{name} is on no remote"))?;
            (remote.to_string(), Page::Branch(branch.to_string()))
        }
        // the pull request of its upstream branch, else of a branch of the same name
        LinkTarget::Branch(branch) => match info.upstreams.get(branch) {
            Some((remote, upstream)) => (remote.clone(), Page::PullRequest(upstream.clone())),
            None => (remote_of(None)?, Page::PullRequest(branch.clone())),
        },
    };
    let url = repository
        .remote_url(&remote)
        .ok_or_else(|| format!("The remote {remote} has no URL"))?;
    web_link(&url, services, &page)
}

fn web_link(
    url: &str,
    services: &FxHashMap<String, Service>,
    page: &Page,
) -> Result<String, String> {
    let (scheme, host, path) =
        parse_remote_url(url).ok_or_else(|| format!("{url} is not on a web host"))?;
    // the host without its port, "git.company.com"
    let domain = host.split(':').next().unwrap_or(&host).to_string();
    let service = services
        .get(&host)
        .or_else(|| services.get(&domain))
        .cloned()
        .or_else(|| guess_kind(&domain).map(|kind| Service::Kind(kind.into())))
        .ok_or_else(|| {
            format!("{domain} is no known host: name its kind in [core.browser.services]")
        })?;
    let (links, web_host) = match service {
        Service::Links(links) => (links, host),
        Service::Kind(kind) => {
            let (kind, web_host) = match kind.split_once(':') {
                Some((kind, web_host)) => (kind.to_string(), web_host.to_string()),
                None => (kind, host),
            };
            let links = Links::of_kind(&kind).ok_or_else(|| {
                format!("{kind} is no kind of host: github, gitlab, bitbucket or gitea")
            })?;
            (links, web_host)
        }
    };
    let base = format!("{scheme}://{web_host}/{path}");
    let (pattern, what) = match page {
        Page::Repo => return Ok(base),
        Page::Commit(_) => (&links.commit, "commit"),
        Page::Branch(_) => (&links.branch, "branch"),
        Page::PullRequest(_) => (&links.pull_request, "pull_request"),
        Page::Tag(_) => (&links.tag, "tag"),
        Page::File { .. } => (&links.file, "file"),
    };
    let pattern = pattern
        .as_ref()
        .ok_or_else(|| format!("{domain} has no {what} link in [core.browser.services]"))?;
    let (hash, name, path) = match page {
        Page::Commit(hash) => (hash.as_str(), "", ""),
        Page::Branch(name) | Page::PullRequest(name) | Page::Tag(name) => ("", name.as_str(), ""),
        Page::File { commit, path } => (commit.as_str(), "", path.as_str()),
        Page::Repo => ("", "", ""),
    };
    Ok(pattern
        .replace("{base}", &base)
        .replace("{hash}", hash)
        .replace("{branch}", &encode(name))
        .replace("{tag}", &encode(name))
        .replace("{path}", &encode(path)))
}

// (scheme of the pages, host with its port when it serves them, owner/repo) of a remote URL:
// https://host/owner/repo.git, ssh://git@host:22/owner/repo.git or git@host:owner/repo.git.
// None for a path on this machine.
fn parse_remote_url(url: &str) -> Option<(&'static str, String, String)> {
    let (scheme, host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            match scheme {
                // the port of ssh is not the one of the pages
                "http" => ("http", host.to_string(), path),
                "https" => ("https", host.to_string(), path),
                "file" => return None,
                _ => ("https", host.split(':').next()?.to_string(), path),
            }
        }
        // scp-like, "git@host:owner/repo"; a local path has a slash before any colon
        None => {
            let (left, path) = url.split_once(':')?;
            if left.contains('/') {
                return None;
            }
            ("https", left.rsplit('@').next()?.to_string(), path)
        }
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| (scheme, host, path.to_string()))
}

fn guess_kind(domain: &str) -> Option<&'static str> {
    [
        "github",
        "gitlab",
        "bitbucket",
        "gitea",
        "forgejo",
        "codeberg",
    ]
    .into_iter()
    .find(|kind| domain.contains(kind))
}

// Percent-encodes all but letters, digits, "-._~" and "/", for branch names and paths in links.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote_url() {
        let parsed = |url| parse_remote_url(url).map(|(s, h, p)| format!("{s}://{h}/{p}"));
        let expected = Some("https://github.com/o/r".to_string());
        assert_eq!(parsed("git@github.com:o/r.git"), expected);
        assert_eq!(parsed("https://github.com/o/r.git"), expected);
        assert_eq!(parsed("https://user:token@github.com/o/r/"), expected);
        assert_eq!(parsed("ssh://git@github.com:22/o/r.git"), expected);
        assert_eq!(
            parsed("http://git.local:3000/team/o/r"),
            Some("http://git.local:3000/team/o/r".into())
        );
        assert_eq!(parsed("/srv/git/r.git"), None);
        assert_eq!(parsed("../r"), None);
        assert_eq!(parsed("file:///srv/git/r.git"), None);
    }

    #[test]
    fn test_web_link() {
        let none = FxHashMap::default();
        let link = |url, services, page| web_link(url, services, &page);
        let url = "git@github.com:o/r.git";
        assert_eq!(
            link(url, &none, Page::Commit("abc".into())),
            Ok("https://github.com/o/r/commit/abc".into())
        );
        assert_eq!(
            link(url, &none, Page::PullRequest("feat/a#1".into())),
            Ok("https://github.com/o/r/compare/feat/a%231?expand=1".into())
        );
        assert_eq!(
            link(
                url,
                &none,
                Page::File {
                    commit: "abc".into(),
                    path: "src/a b.rs".into()
                }
            ),
            Ok("https://github.com/o/r/blob/abc/src/a%20b.rs".into())
        );
        assert_eq!(
            link(url, &none, Page::Repo),
            Ok("https://github.com/o/r".into())
        );
        assert_eq!(
            link("git@gitlab.com:g/s/r.git", &none, Page::Tag("v1".into())),
            Ok("https://gitlab.com/g/s/r/-/tags/v1".into())
        );

        // unknown hosts, then mapped: by kind, by kind and domain, by patterns
        let url = "git@git.company.com:o/r.git";
        assert!(link(url, &none, Page::Repo).is_err());
        let services = FxHashMap::from_iter([
            ("git.company.com".into(), Service::Kind("gitea".into())),
            (
                "github.com-work".into(),
                Service::Kind("github:github.com".into()),
            ),
            (
                "code.example.org".into(),
                Service::Links(Links {
                    commit: Some("{base}/c/{hash}".into()),
                    branch: None,
                    pull_request: None,
                    tag: None,
                    file: None,
                }),
            ),
        ]);
        assert_eq!(
            link(url, &services, Page::Branch("main".into())),
            Ok("https://git.company.com/o/r/src/branch/main".into())
        );
        assert_eq!(
            link(
                "git@github.com-work:o/r.git",
                &services,
                Page::Commit("abc".into())
            ),
            Ok("https://github.com/o/r/commit/abc".into())
        );
        let url = "https://code.example.org/o/r";
        assert_eq!(
            link(url, &services, Page::Commit("abc".into())),
            Ok("https://code.example.org/o/r/c/abc".into())
        );
        assert!(link(url, &services, Page::Tag("v1".into())).is_err());
    }
}
