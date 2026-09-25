//! `bn forge`: the Forge app (forge.wira360.com) as bn's task store, plus the local per-agent
//! worker bridge (list/claim/dispatch/tick) that runs claimed tasks through `bn dispatch`.
//!
//! Named `forge_app` in code because `bn_core::forge` is the unrelated git-host abstraction. Both
//! halves share the one `bn forge` word: [`ForgeAppCmd::Login`]/[`ForgeAppCmd::Status`]/
//! [`ForgeAppCmd::Sweep`] administer the family-wide app, and the rest drive one local agent
//! profile's claims — see [`crate::cmd::forge`] for the worker logic itself.
use crate::*;

#[derive(Subcommand, Debug)]
pub(crate) enum ForgeAppCmd {
    /// Mint a Forge key for the family's agent member and save it to `<config_dir>/forge-token`.
    /// Opens Forge in a browser with a persistent profile (via `playwright-cli`), waits for you to
    /// sign in, finds or creates the agent member, and runs `createAgentApiKey` inside the page, so
    /// your session authorises it. The key is never printed. `--paste` reads a key from stdin
    /// instead (for example one copied from Forge's Settings page).
    Login {
        /// Read the key from stdin instead of driving a browser.
        #[arg(long)]
        paste: bool,
        /// The Forge web app to open.
        #[arg(long, default_value = "https://forge.wira360.com")]
        web_url: String,
        /// The Forge API origin for GraphQL. Defaults to `[tasks] forge_url` without its `/mcp`.
        #[arg(long)]
        api_url: Option<String>,
        /// The family to mint the key in, by name. Needed only when you administer more than one.
        #[arg(long)]
        family: Option<String>,
        /// The agent member's name, used when the family has none yet.
        #[arg(long, default_value = "Agents")]
        agent_name: String,
        /// `playwright-cli --browser` channel: chrome or msedge.
        #[arg(long, default_value = "chrome")]
        browser: String,
        /// Seconds to wait for you to finish signing in.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Create the agent member without asking when the family has none.
        #[arg(long)]
        yes: bool,
    },
    /// Where the key comes from (never the key), whether the MCP session opens, the projects goals
    /// with their repositories, and how many agent tasks are open and claimed.
    Status,
    /// Run the Forge agent loop once: claim open tasks assigned to the agent member whose
    /// repository is cloned and allowed here, start each claim with `bn dispatch --task`, then
    /// complete tasks whose PR has merged. Nothing runs it automatically; run it by hand or from
    /// your own cron. Each step does its work at most once a minute.
    Sweep {
        /// The most unfinished claims this machine holds at once (claimed, no PR yet).
        #[arg(long, default_value_t = 1)]
        max: usize,
    },
    /// List claimable Forge bn tasks for a local agent profile.
    ///
    /// Reads `~/.config/wira360/agents/<agent>.json`, validates the OAuth-only MCP profile, then
    /// calls Forge MCP `list_bn_tasks` with `claimable=true`. This is read-only: it never claims,
    /// writes notes, or dispatches work.
    List {
        /// Local Forge agent profile name.
        agent: String,
        /// Show one task id from the claimable set.
        #[arg(long)]
        task: Option<String>,
        /// Maximum rows to print.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Claim exactly one Forge bn task for the profile's claimer.
    Claim {
        /// Local Forge agent profile name.
        agent: String,
        /// Forge activity/task id to claim. Required so bn never guesses among multiple tasks.
        #[arg(long)]
        task: String,
        /// Forge claim lease length in seconds.
        #[arg(long, default_value_t = 900)]
        lease_seconds: u64,
    },
    /// Claim one Forge bn task, then dispatch it through existing `bn dispatch`.
    Dispatch {
        /// Local Forge agent profile name.
        agent: String,
        /// Forge activity/task id to claim before dispatch.
        #[arg(long)]
        task: String,
        /// Target bn repo. Defaults to the current worktree repo when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Harness passed to `bn dispatch --agent`.
        #[arg(long = "agent", default_value = "copilot")]
        agent_harness: String,
        /// Base branch passed to `bn dispatch --base`.
        #[arg(long)]
        base: Option<String>,
        /// Workflow skill passed to `bn dispatch --skill`.
        #[arg(long, default_value = "forge-task")]
        skill: String,
        /// Readable task name for the local bn agent.
        #[arg(long)]
        name: Option<String>,
        /// Gate the dispatched agent on `bn verify`.
        #[arg(long)]
        verify: bool,
        /// Forge claim lease length in seconds.
        #[arg(long, default_value_t = 900)]
        lease_seconds: u64,
    },
    /// Poll Forge once: reap abandoned claims, then dispatch newly assigned work.
    ///
    /// The unattended half of `bn forge`. Every in-flight task this agent still holds is checked
    /// against the live fleet: a claim whose worker is gone is released back to Forge rather than
    /// held forever, and a claim whose worker is alive gets its lease extended. Remaining capacity
    /// is then filled from the claimable queue. Designed to run from cron every ~10 minutes.
    Tick {
        /// Local Forge agent profile name.
        agent: String,
        /// Target bn repo. Defaults to the current worktree repo when omitted.
        #[arg(long)]
        repo: Option<String>,
        /// Harness passed to `bn dispatch --agent`.
        #[arg(long = "agent", default_value = "copilot")]
        agent_harness: String,
        /// Base branch passed to `bn dispatch --base`.
        #[arg(long)]
        base: Option<String>,
        /// Workflow skill passed to `bn dispatch --skill`.
        #[arg(long, default_value = "forge-task")]
        skill: String,
        /// Maximum tasks this agent may have in flight at once.
        #[arg(long, default_value_t = 2)]
        max_active: usize,
        /// Report what the tick would do without claiming, dispatching, or releasing anything.
        #[arg(long)]
        dry_run: bool,
        /// Forge claim lease length in seconds.
        #[arg(long, default_value_t = 900)]
        lease_seconds: u64,
    },
}
