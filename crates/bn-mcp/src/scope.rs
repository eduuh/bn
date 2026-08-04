//! [`Scope`] — how a single MCP request decides which branch note it acts on.
//!
//! The MCP tool-router is served over a shared HTTP transport where each request must carry its
//! own `(repo, branch)`. `Scope` is the seam: a tool resolves its note context *per call* from the
//! scope it was handed, never from a process-global directory. The shared server is built with
//! [`Scope::Request`] and derives a per-call [`Scope::Target`] from the request's headers/query;
//! resolution funnels through [`resolver::resolve_target`], so the fail-closed "not in a git repo"
//! boundary and the note-key normalization stay in one place.

use std::path::{Path, PathBuf};

use bn_core::resolver::{self, Config, ContextError, Resolved};

/// The `(repo, branch)` a request operates on — carried explicitly per request.
#[derive(Debug, Clone)]
pub enum Scope {
    /// An explicit `(repo, branch)` pair supplied per request. It maps straight onto the note key
    /// (via the same normalization as the CLI's explicit targets) with no git dependency, so a
    /// shared transport can serve any branch without a matching cwd.
    Target { repo: String, branch: String },
    /// A fixed launch worktree: the scope is derived once from `dir` via the git-backed
    /// [`resolver::resolve`], exactly as the CLI resolves the current worktree. This is how the
    /// per-worktree **stdio** launcher (`bn mcp serve`) pins itself to the directory it was started
    /// in, so a plain MCP client needs no per-call scope headers — the branch identity comes from
    /// cwd, the standard MCP pattern.
    Cwd { dir: PathBuf },
    /// The scope is carried by the incoming HTTP request: each tool call derives its
    /// `(repo, branch)` from that request's `X-Bn-Repo`/`X-Bn-Branch` headers or `?repo=&branch=`
    /// query, failing closed when absent. A server built with this variant never acts on a fixed
    /// note — the HTTP transport re-derives a per-call [`Scope::Target`] and hands it to
    /// [`Scope::resolve`], so a single shared server serves every branch without leaking one
    /// request's scope into another. Resolving this variant directly is an error: it only names
    /// *where* the scope comes from, and the concrete pair lives on the request.
    Request,
}

impl Scope {
    /// An explicit `(repo, branch)` scope.
    pub fn target(repo: impl Into<String>, branch: impl Into<String>) -> Self {
        Scope::Target {
            repo: repo.into(),
            branch: branch.into(),
        }
    }

    /// A scope pinned to a launch directory, resolved per call via the git-backed
    /// [`resolver::resolve`]. Used by the stdio launcher to lock onto its worktree.
    pub fn launch(dir: impl Into<PathBuf>) -> Self {
        Scope::Cwd { dir: dir.into() }
    }

    /// Resolve this scope to a note context, failing closed on a missing/invalid scope. A
    /// [`Scope::Target`] normalizes its explicit pair with no git dependency; a [`Scope::Cwd`]
    /// resolves its launch directory through git exactly as the CLI does.
    pub fn resolve(&self, cfg: &Config) -> Result<Resolved, ContextError> {
        match self {
            Scope::Target { repo, branch } => resolver::resolve_target(
                cfg,
                Path::new("."),
                Some(repo.clone()),
                Some(branch.clone()),
            ),
            Scope::Cwd { dir } => resolver::resolve(cfg, dir),
            // A `Request` scope carries no concrete pair — the HTTP transport re-derives a
            // per-call `Target` from the request and resolves that instead. Reaching here means a
            // server was handed the marker without a request to read, so it fails closed.
            Scope::Request => Err(ContextError::NotInGitRepo),
        }
    }
}


