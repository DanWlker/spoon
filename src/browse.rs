// Links to the pages of a commit, a branch, a tag or a file on the web host of a remote, like
// lazygit's "open in browser": the remote URL becomes https://host/owner/repo, and each kind of
// host lays out its pages its own way.

use std::ops::RangeInclusive;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::{
    event::UserEvent,
    git::{CommitHash, RemoteBranch, Repository},
};

// What a link is for: opened in the browser (o), or copied (O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAction {
    Open,
    Copy,
}

impl LinkAction {
    // Of o or O.
    pub fn of(event: UserEvent) -> Self {
        if event == UserEvent::CopyLink {
            LinkAction::Copy
        } else {
            LinkAction::Open
        }
    }
}

// What to link to, from the selected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Commit(CommitHash),
    // a local branch: its page, or a new pull request from it, on the remote of its upstream
    Branch(String),
    // "origin/main"
    RemoteBranch(String),
    Tag(String),
    // lines, when a hunk or line of the diff or a line of the file is selected
    File {
        commit: CommitHash,
        path: String,
        lines: Option<RangeInclusive<usize>>,
    },
    Remote(String),
}

// A host of `[core.browser.services]`: a kind ("gitlab"), the kind and the domain of its pages
// ("github:github.com", for an ssh alias), or its own link patterns.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Forge {
    Kind(String),
    Links(Links),
}

// The patterns of the links of a host: {base} is https://host/owner/repo, and {hash}, {branch},
// {tag} and {path} what is linked to; `line` and `lines` are added to the file's link for a line,
// {start}, or the lines from {start} to {end}.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Links {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub pull_request: Option<String>,
    pub tag: Option<String>,
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<String>,
    #[serde(default)]
    pub lines: Option<String>,
}

const GITHUB: [&str; 7] = [
    "{base}/commit/{hash}",
    "{base}/tree/{branch}",
    "{base}/compare/{branch}?expand=1",
    "{base}/releases/tag/{tag}",
    "{base}/blob/{hash}/{path}",
    "#L{start}",
    "#L{start}-L{end}",
];
const GITLAB: [&str; 7] = [
    "{base}/-/commit/{hash}",
    "{base}/-/tree/{branch}",
    "{base}/-/merge_requests/new?merge_request[source_branch]={branch}",
    "{base}/-/tags/{tag}",
    "{base}/-/blob/{hash}/{path}",
    "#L{start}",
    "#L{start}-{end}",
];
const BITBUCKET: [&str; 7] = [
    "{base}/commits/{hash}",
    "{base}/branch/{branch}",
    "{base}/pull-requests/new?source={branch}&t=1",
    "{base}/src/{tag}",
    "{base}/src/{hash}/{path}",
    "#lines-{start}",
    "#lines-{start}:{end}",
];
const GITEA: [&str; 7] = [
    "{base}/commit/{hash}",
    "{base}/src/branch/{branch}",
    "{base}/compare/{branch}",
    "{base}/src/tag/{tag}",
    "{base}/src/commit/{hash}/{path}",
    "#L{start}",
    "#L{start}-L{end}",
];

impl Links {
    fn of_kind(kind: &str) -> Option<Links> {
        let [commit, branch, pull_request, tag, file, line, lines] = match kind {
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
            line: some(line),
            lines: some(lines),
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
    File {
        commit: String,
        path: String,
        lines: Option<RangeInclusive<usize>>,
    },
}

// A link to choose from: the remote it is on, what it shows ("Branch") and its URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLink {
    pub remote: String,
    pub page: String,
    pub url: String,
}

// The links of `target` to choose from, on the web host of each remote it may be on: a local
// branch's page and a new pull request from it, else the one page. A remote not on a web host, like
// a path, is left out, failing only when none is.
pub fn links(
    repository: &Repository,
    services: &FxHashMap<String, Forge>,
    target: &LinkTarget,
) -> Result<Vec<PageLink>, String> {
    let info = repository.remote_info();
    // the default remote of the branch, then the others
    let remotes_of = |branch: Option<&str>| -> Result<Vec<String>, String> {
        let first = info
            .default_remote(branch)
            .ok_or_else(|| "There is no remote to link to".to_string())?;
        let others = info.remotes.iter().filter(|r| *r != first);
        Ok(std::iter::once(first).chain(others).cloned().collect())
    };
    let current = info.current_branch.as_deref();
    let (remotes, pages) = match target {
        LinkTarget::Commit(hash) => (
            remotes_of(current)?,
            vec![("Commit", Page::Commit(hash.as_str().to_string()))],
        ),
        LinkTarget::File {
            commit,
            path,
            lines,
        } => (
            remotes_of(current)?,
            vec![(
                "File",
                Page::File {
                    commit: commit.as_str().to_string(),
                    path: path.clone(),
                    lines: lines.clone(),
                },
            )],
        ),
        LinkTarget::Tag(tag) => (remotes_of(current)?, vec![("Tag", Page::Tag(tag.clone()))]),
        LinkTarget::Remote(remote) => (vec![remote.clone()], vec![("Repository", Page::Repo)]),
        LinkTarget::RemoteBranch(name) => {
            let RemoteBranch { remote, branch } = RemoteBranch::parse(&info.remotes, name)
                .ok_or_else(|| format!("{name} is on no remote"))?;
            (vec![remote], vec![("Branch", Page::Branch(branch))])
        }
        // its upstream branch, else a branch of the same name
        LinkTarget::Branch(branch) => {
            let (remotes, name) = match info.upstreams.get(branch) {
                Some(upstream) => (vec![upstream.remote.clone()], upstream.branch.clone()),
                None => (remotes_of(None)?, branch.clone()),
            };
            let pages = vec![
                ("Branch", Page::Branch(name.clone())),
                ("Pull request", Page::PullRequest(name)),
            ];
            (remotes, pages)
        }
    };
    let mut links = vec![];
    let mut error = None;
    for remote in remotes {
        // read for each remote, not each page: `get-url` rewrites it by `url.*.insteadOf`
        let url = repository.remote_url(&remote);
        let made: Result<Vec<PageLink>, String> = pages
            .iter()
            .map(|(label, page)| {
                let url = url
                    .as_deref()
                    .ok_or_else(|| format!("The remote {remote} has no URL"))?;
                Ok(PageLink {
                    remote: remote.clone(),
                    page: label.to_string(),
                    url: web_link(url, services, page)?,
                })
            })
            .collect();
        match made {
            Ok(made) => links.extend(made),
            Err(err) => {
                error.get_or_insert(err);
            }
        }
    }
    match error {
        Some(err) if links.is_empty() => Err(err),
        _ => Ok(links),
    }
}

fn web_link(url: &str, services: &FxHashMap<String, Forge>, page: &Page) -> Result<String, String> {
    let RemoteUrl { scheme, host, path } =
        parse_remote_url(url).ok_or_else(|| format!("{url} is not on a web host"))?;
    // the host without its port, "git.company.com"
    let domain = host.split(':').next().unwrap_or(&host).to_string();
    let service = services
        .get(&host)
        .or_else(|| services.get(&domain))
        .cloned()
        .or_else(|| guess_kind(&domain).map(|kind| Forge::Kind(kind.into())))
        .ok_or_else(|| {
            format!("{domain} is no known host: name its kind in [core.browser.services]")
        })?;
    let (links, web_host) = match service {
        Forge::Links(links) => (links, host),
        Forge::Kind(kind) => {
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
        Page::File { commit, path, .. } => (commit.as_str(), "", path.as_str()),
        Page::Repo => ("", "", ""),
    };
    let link = pattern
        .replace("{base}", &base)
        .replace("{hash}", hash)
        .replace("{branch}", &encode(name))
        .replace("{tag}", &encode(name))
        .replace("{path}", &encode(path));
    // the selected lines, where the host's links can point at them
    let fragment = match page {
        Page::File {
            lines: Some(selected),
            ..
        } => match (&links.line, &links.lines) {
            (Some(line), _) if selected.start() == selected.end() => line.clone(),
            (_, Some(lines)) => lines.clone(),
            (Some(line), None) => line.clone(),
            (None, None) => String::new(),
        }
        .replace("{start}", &selected.start().to_string())
        .replace("{end}", &selected.end().to_string()),
        _ => String::new(),
    };
    Ok(link + &fragment)
}

// Where a remote's repository is on its web host: the scheme of the pages, the host with its port
// when it serves them, and "owner/repo".
#[derive(Debug, PartialEq, Eq)]
struct RemoteUrl {
    scheme: &'static str,
    host: String,
    path: String,
}

// Where the repository of a remote URL is on the web: https://host/owner/repo.git,
// ssh://git@host:22/owner/repo.git or git@host:owner/repo.git. None for a path on this machine.
fn parse_remote_url(url: &str) -> Option<RemoteUrl> {
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
    (!host.is_empty() && !path.is_empty()).then(|| RemoteUrl {
        scheme,
        host,
        path: path.to_string(),
    })
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

// All but letters, digits, "-._~" and "/" are percent-encoded in branch names and paths in links.
const LINK_PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'/');

fn encode(text: &str) -> String {
    utf8_percent_encode(text, LINK_PATH).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode() {
        // UTF-8 bytes each, "-._~" and "/" as they are
        assert_eq!(encode("feat/é #1-a.b_c~"), "feat/%C3%A9%20%231-a.b_c~");
    }

    #[test]
    fn test_parse_remote_url() {
        let parsed = |url| {
            parse_remote_url(url)
                .map(|RemoteUrl { scheme, host, path }| format!("{scheme}://{host}/{path}"))
        };
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
                    path: "src/a b.rs".into(),
                    lines: None,
                }
            ),
            Ok("https://github.com/o/r/blob/abc/src/a%20b.rs".into())
        );
        // the selected lines, as each host points at them
        let file = |lines| Page::File {
            commit: "abc".into(),
            path: "a.rs".into(),
            lines: Some(lines),
        };
        assert_eq!(
            link(url, &none, file(5..=5)),
            Ok("https://github.com/o/r/blob/abc/a.rs#L5".into())
        );
        assert_eq!(
            link(url, &none, file(5..=9)),
            Ok("https://github.com/o/r/blob/abc/a.rs#L5-L9".into())
        );
        assert_eq!(
            link("git@gitlab.com:o/r.git", &none, file(5..=9)),
            Ok("https://gitlab.com/o/r/-/blob/abc/a.rs#L5-9".into())
        );
        assert_eq!(
            link("git@bitbucket.org:o/r.git", &none, file(5..=9)),
            Ok("https://bitbucket.org/o/r/src/abc/a.rs#lines-5:9".into())
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
            ("git.company.com".into(), Forge::Kind("gitea".into())),
            (
                "github.com-work".into(),
                Forge::Kind("github:github.com".into()),
            ),
            (
                "code.example.org".into(),
                Forge::Links(Links {
                    commit: Some("{base}/c/{hash}".into()),
                    branch: None,
                    pull_request: None,
                    tag: None,
                    file: None,
                    line: None,
                    lines: None,
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
