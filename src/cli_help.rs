//! The CLI's command surface: the top-level command list and one help page per
//! command, reachable as `jjfx help <command>`. A multi-word group (`ticket`)
//! answers with its subcommands too, derived from the same top-level table, so
//! `jjfx help ticket` never reports the group as an unknown command.
//!
//! The pages and the parser read the *same* flag definitions. Each command's
//! `flags` slice is the exact one `cli::reject_unknown_flags` validates
//! against, so a flag the parser handles but a page omits (or the reverse)
//! cannot be accepted - the parser only ever accepts a flag some page
//! documents. The tests drive `cli::parse` from the pages to keep that honest.

use anyhow::Result;

const PROGRAM: &str = env!("CARGO_PKG_NAME");

/// One command-line flag: the single definition a help page and the parser
/// both read, so the two cannot drift.
pub(crate) struct Flag {
    /// Canonical long spelling, e.g. `--provider`.
    pub(crate) long: &'static str,
    /// Short alias, when the parser accepts one.
    pub(crate) short: Option<&'static str>,
    /// Value placeholder when the flag takes a value, e.g. `PROVIDER`.
    pub(crate) value: Option<&'static str>,
    /// One-line description for the help page.
    pub(crate) help: &'static str,
}

impl Flag {
    /// Whether `argument` spells this flag in either its long or short form.
    pub(crate) fn matches(&self, argument: &str) -> bool {
        argument == self.long || self.short == Some(argument)
    }

    /// The spelling shown on a help page, e.g. `-r, --revision <REV>`.
    fn rendered(&self) -> String {
        let mut out = String::new();
        if let Some(short) = self.short {
            out.push_str(short);
            out.push_str(", ");
        }
        out.push_str(self.long);
        if let Some(value) = self.value {
            out.push_str(&format!(" <{value}>"));
        }
        out
    }
}

/// One positional argument a command accepts.
struct Argument {
    name: &'static str,
    help: &'static str,
}

/// One command's help page.
struct CommandHelp {
    /// The command words, which is also the `jjfx help <key>` key.
    key: &'static str,
    /// One-line description, shown under the heading.
    summary: &'static str,
    /// The usage line, without the leading `Usage: jjfx `.
    usage: &'static str,
    arguments: &'static [Argument],
    /// The flags the parser validates this command against.
    flags: &'static [Flag],
}

impl CommandHelp {
    /// The page as plain text: a heading, the usage line, then the arguments
    /// and flags the command accepts.
    fn render(&self) -> String {
        let mut out = format!(
            "{PROGRAM} {} - {}\n\nUsage: {PROGRAM} {}\n",
            self.key, self.summary, self.usage
        );
        if !self.arguments.is_empty() {
            out.push_str("\nArguments:\n");
            for argument in self.arguments {
                out.push_str(&format!("  {:<24} {}\n", argument.name, argument.help));
            }
        }
        if !self.flags.is_empty() {
            out.push_str("\nFlags:\n");
            for flag in self.flags {
                out.push_str(&format!("  {:<24} {}\n", flag.rendered(), flag.help));
            }
        }
        out.push_str(notes(self.key));
        out
    }
}

/// Extra prose for a page, beyond its usage, arguments and flags.
///
/// A couple of pages state something the flag list cannot - the conditions and
/// exit codes `wait` returns, how `dispatch --fg` relates to `wait`. Kept out of
/// [`CommandHelp`] so the many pages without notes need not spell out an empty
/// one.
fn notes(key: &str) -> &'static str {
    match key {
        "wait" => {
            "\nConditions (--until):\n  done              the Worker succeeded\n  failed            the Worker failed\n  terminal          the Worker reached any terminal state (default)\n  ticket-resolved   the Worker's local Ticket is resolved\n\nExit codes:\n  0  the condition was reached\n  1  timed out: the Worker is not done\n  2  a Worker reached a terminal state that does not satisfy --until\n\nA Repository with one Worker Pool implies it, so `--pool` is optional. A\nRepository with several requires it: an unqualified wait refuses, naming the\npools that exist, rather than acting on `default` alone. `--all` then means\nevery Worker in the selected pool.\n"
        }
        "dispatch" => {
            "\n--fg blocks until every Run this Dispatch started has finished, inside\nthat one call. `jjfx wait` re-attaches to a Worker any session started, and\ncarries the answer in its exit code.\n\nA Repository with one Worker Pool implies it, so `--pool` is optional. A\nRepository with several requires it: an unqualified Dispatch refuses, naming\nthe pools that exist, rather than choosing one.\n\nThe exception is `--all`, which fans out across the pools that declare a\nrouting label (`[pools.bugs] label = \"bug\"`) and sends each Ticket to the pool\nwhose label it carries. A Ticket matching no pool is reported, not dropped,\nand an explicit `--pool` overrides routing to mean only that lane.\n"
        }
        "resume" => {
            "\nAn interruption leaves the work it had in flight recorded in the pool's\ndurable queue. `resume` re-runs that work: it reads the queue's per-step\nstate, so a step that reached `done` is left alone rather than redone, and a\nstep whose Run is still live is left running. Everything else - accepted and\nwaiting, claimed without a Run, failed, or abandoned - is re-opened and\ndispatched again, with the same capacity governor, spend limits and\noccupancy exclusions a Dispatch obeys.\n\nResumption is explicit rather than an automatic retry, because re-running a\nstep may have side effects. Naming Tickets narrows it to those steps; a Ticket\nwith no recorded step is reported, not dispatched (`jjfx dispatch` runs new\nwork). A Repository with one Worker Pool implies it, so `--pool` is optional,\nand one with several requires it, exactly as `dispatch` does.\n\nThe queue is reconciled first, so a Run that ended is settled and a Run still\nlive is recognised. To abandon a Run the queue still believes is live, reset\nits Worker (`jjfx pool reset <worker>`) and then resume.\n"
        }
        "review" => {
            "\n`review` sends one Follow-up prompt that addresses the pull request's\nreview state once. It runs no reviewer and no bounded loop: jjfx has no\nreviewer of its own (ADR 0016 cut review-bot ingestion), so no command\ndrives `ReviewLoop` - the loop and its decision log stand ready for the\nharness, which is their only caller. `--fg` selects when this Run returns,\nnot how many passes run.\n"
        }
        "sync" => {
            "\n`sync` moves the working copy in the current directory onto trunk: it\nfetches first when the repository has a remote, then replaces an empty\nworking copy with a fresh commit above trunk (`jj new`) or rebases one that\ncarries changes (`jj rebase`). A rebase that conflicts is reported, not\nresolved - run `jjfx resolve` on the workspace, or `jj resolve`, and\ncontinue.\n\nNot to be confused with:\n  refresh          rebuild the workspace cache; nothing moves\n  rebase <worker>  rebase and push a named Worker's bookmark\n"
        }
        "ticket list" => {
            "\nEvery Ticket the configured source discovers is listed with its `Status`,\nits Blockers and its state: `ready` (a Dispatch would take it now), `blocked`\n(something stops it, and the reason names what) or `done` (resolved). The\nblocked reason is the same line `jjfx dispatch --all` prints, because both read\none verdict.\n\nThe lanes are the lanes a `dispatch --all` would fill: several routable pools\nfan out one lane each, and `--pool` narrows it to one. A Repository that holds\nseveral pools and declares no routing labels requires `--pool`, exactly as an\nunqualified Dispatch does.\n\nThe command is read-only: it reconciles no Run and writes no Ticket, so a\n`--json` consumer can poll it. Use `jjfx ticket reconcile` (or a Dispatch) to\nsettle finished Runs.\n"
        }
        "children" => {
            "\nA parent reaches this from its own shell: opencode puts the session's id\nin OPENCODE_SESSION_ID, so a bare `jjfx children` lists the sessions that\nsession dispatched, and naming one lists another session's. It needs no jj\nrepository, so a parent can ask from anywhere.\n\nThe children are the union of what opencode reports and what the lifecycle\nlog has seen; each state is the fold's. opencode also supplies each child's\ntitle and directory - which Worker it is - when it is reachable, and the log\nalone still names every child and its state when it is not.\n"
        }
        _ => "",
    }
}

/// One line in the top-level command list.
struct Entry {
    /// The command words, which is also the `jjfx help <key>` key.
    key: &'static str,
    /// The arguments and flags shown after the command words.
    args: &'static str,
    summary: &'static str,
}

/// A titled block of the top-level command list. The first group is
/// unlabelled and sits directly under `Usage:`.
struct Group {
    title: Option<&'static str>,
    entries: &'static [Entry],
}

/// No flags: a command whose page lists only a usage line and arguments.
const NO_FLAGS: &[Flag] = &[];

// Flags shared verbatim by more than one command. A command's page and its
// parser read the same const, so the two can never disagree about the flag.
const FG: Flag = Flag {
    long: "--fg",
    short: None,
    value: None,
    help: "Wait for each Run to finish before returning",
};
const BG: Flag = Flag {
    long: "--bg",
    short: None,
    value: None,
    help: "Return once each Run's pid is persisted (default)",
};

pub(crate) const ADD_FLAGS: &[Flag] = &[Flag {
    long: "--revision",
    short: Some("-r"),
    value: Some("REV"),
    help: "Create the workspace at this revision",
}];

pub(crate) const RM_FLAGS: &[Flag] = &[Flag {
    long: "--force",
    short: Some("-f"),
    value: None,
    help: "Remove even a workspace with uncommitted work",
}];

pub(crate) const CLEAN_FLAGS: &[Flag] = &[Flag {
    long: "--pool",
    short: None,
    value: Some("NAME"),
    help: "Remove only this Worker Pool's workspaces (default: every workspace)",
}];

pub(crate) const STATUS_FLAGS: &[Flag] = &[Flag {
    long: "--json",
    short: None,
    value: None,
    help: "Emit machine-readable JSON",
}];

pub(crate) const CHILDREN_FLAGS: &[Flag] = &[Flag {
    long: "--json",
    short: None,
    value: None,
    help: "Emit machine-readable JSON",
}];

/// `ticket list` is a read-only read, so it takes only what narrows the lane and
/// the output format.
pub(crate) const TICKET_LIST_FLAGS: &[Flag] = &[
    Flag {
        long: "--label",
        short: None,
        value: Some("LABEL"),
        help: "Ticket label to discover by (default: the lane's routing label, else ready-for-agent)",
    },
    Flag {
        long: "--pool",
        short: None,
        value: Some("NAME"),
        help: "Evaluate only this Worker Pool's lane",
    },
    Flag {
        long: "--json",
        short: None,
        value: None,
        help: "Emit machine-readable JSON",
    },
];

pub(crate) const DISPATCH_FLAGS: &[Flag] = &[
    FG,
    BG,
    Flag {
        long: "--all",
        short: None,
        value: None,
        help: "Dispatch every discovered Ready Ticket, routing each to its pool by label",
    },
    Flag {
        long: "--text",
        short: None,
        value: Some("TEXT"),
        help: "Dispatch a one-off Ticket whose body is this text (no tracker)",
    },
    Flag {
        long: "--from-file",
        short: None,
        value: Some("PATH"),
        help: "Dispatch a Ticket from any markdown file (use `-` for stdin)",
    },
    Flag {
        long: "--id",
        short: None,
        value: Some("ID"),
        help: "Ticket id for --text/--from-file (default derived from the input)",
    },
    Flag {
        long: "--label",
        short: None,
        value: Some("LABEL"),
        help: "Ticket label for `--all` discovery (default: the pool's label, else ready-for-agent)",
    },
    Flag {
        long: "--pool",
        short: None,
        value: Some("NAME"),
        help: "Worker Pool to fill; an explicit one means only this lane",
    },
    Flag {
        long: "--provider",
        short: None,
        value: Some("PROVIDER"),
        help: "Override the pool profile's model provider",
    },
    Flag {
        long: "--model",
        short: None,
        value: Some("MODEL"),
        help: "Override the pool profile's model",
    },
    Flag {
        long: "--budget",
        short: None,
        value: Some("USD"),
        help: "Cap this Dispatch's spend in whole dollars",
    },
    Flag {
        long: "--no-orchestrate",
        short: None,
        value: None,
        help: "Dispatch the Ticket directly, skipping Parent orchestration",
    },
    Flag {
        long: "--force",
        short: None,
        value: None,
        help: "Dispatch even to a Worker whose Workspace is open",
    },
    Flag {
        long: "--json",
        short: None,
        value: None,
        help: "Print one machine-readable object per Ticket instead of prose",
    },
];

pub(crate) const SEND_FLAGS: &[Flag] = &[
    FG,
    BG,
    Flag {
        long: "--force",
        short: None,
        value: None,
        help: "Send even to a Worker whose Workspace is open",
    },
];

pub(crate) const REVIEW_FLAGS: &[Flag] = &[
    Flag {
        long: "--fg",
        short: None,
        value: None,
        help: "Wait for the review Run before returning",
    },
    Flag {
        long: "--bg",
        short: None,
        value: None,
        help: "Return once the review Run's pid is persisted (default)",
    },
];

pub(crate) const RESOLVE_FLAGS: &[Flag] = &[Flag {
    long: "--tool",
    short: Some("-t"),
    value: Some("NAME"),
    help: "Merge tool passed to `jj resolve` (for example `:ours`)",
}];

pub(crate) const UNDO_FLAGS: &[Flag] = &[Flag {
    long: "--force",
    short: Some("-f"),
    value: None,
    help: "Discard the later operations a restore would also roll back",
}];

pub(crate) const POOL_LIST_FLAGS: &[Flag] = &[Flag {
    long: "--json",
    short: None,
    value: None,
    help: "Emit machine-readable JSON",
}];

pub(crate) const POOL_PROFILE_FLAGS: &[Flag] = &[
    Flag {
        long: "--provider",
        short: None,
        value: Some("PROVIDER"),
        help: "Model provider for the profile",
    },
    Flag {
        long: "--model",
        short: None,
        value: Some("MODEL"),
        help: "Model for the profile",
    },
];

pub(crate) const POOL_RESIZE_FLAGS: &[Flag] = &[
    Flag {
        long: "--size",
        short: Some("-s"),
        value: Some("N"),
        help: "Pool size; the positional `<N>` is the usual form",
    },
    Flag {
        long: "--pool",
        short: None,
        value: Some("NAME"),
        help: "Size this named pool instead of `default`",
    },
];

/// `pool create` takes the `[pools.<name>]` keys, one flag each. Every one is
/// optional, so a bare `pool create bugs` writes a bare `[pools.bugs]`.
pub(crate) const POOL_CREATE_FLAGS: &[Flag] = &[
    Flag {
        long: "--size",
        short: Some("-s"),
        value: Some("N"),
        help: "The number of Workers the pool holds",
    },
    Flag {
        long: "--profile",
        short: None,
        value: Some("NAME"),
        help: "An Agent Runtime Profile the user config defines",
    },
    Flag {
        long: "--source",
        short: None,
        value: Some("SOURCE"),
        help: "Which configured Ticket source discovers this pool's work (linear|local)",
    },
    Flag {
        long: "--label",
        short: None,
        value: Some("LABEL"),
        help: "The Ticket label that routes a Ticket to this pool",
    },
    Flag {
        long: "--ready-status",
        short: None,
        value: Some("STATUS"),
        help: "The Status: value marking a Ticket ready for this pool",
    },
    Flag {
        long: "--cap-usd",
        short: None,
        value: Some("USD"),
        help: "The pool's spend cap in US dollars, under the global ceiling",
    },
    Flag {
        long: "--window",
        short: None,
        value: Some("WINDOW"),
        help: "The window the pool's cap totals over (today|week|total)",
    },
    Flag {
        long: "--on-exceed",
        short: None,
        value: Some("MODE"),
        help: "What Dispatch does once the pool's cap is reached (block|warn)",
    },
];

/// `pool destroy` takes the pool name and `--force`, which discards work.
pub(crate) const POOL_DESTROY_FLAGS: &[Flag] = &[Flag {
    long: "--force",
    short: Some("-f"),
    value: None,
    help: "Remove even a busy pool, discarding undelivered work",
}];

pub(crate) const WAIT_FLAGS: &[Flag] = &[
    Flag {
        long: "--until",
        short: None,
        value: Some("CONDITION"),
        help: "Condition to wait for: done, failed, terminal, ticket-resolved (default terminal)",
    },
    Flag {
        long: "--timeout",
        short: None,
        value: Some("SECS"),
        help: "Give up after this many seconds and exit 1",
    },
    Flag {
        long: "--all",
        short: None,
        value: None,
        help: "Wait for every Worker in the pool",
    },
    Flag {
        long: "--pool",
        short: None,
        value: Some("NAME"),
        help: "Worker Pool to wait on; an explicit one means only this lane",
    },
    Flag {
        long: "--json",
        short: None,
        value: None,
        help: "Print the target Workers as JSON",
    },
];

/// `resume` re-runs the work the durable queue records as unfinished. It takes
/// no `--until`/`--all`: with no Ticket named it resumes every unfinished step,
/// so the bare command is the recovery an operator runs after an interruption.
pub(crate) const RESUME_FLAGS: &[Flag] = &[
    FG,
    BG,
    Flag {
        long: "--pool",
        short: None,
        value: Some("NAME"),
        help: "Worker Pool whose queue to resume; an explicit one means only this lane",
    },
    Flag {
        long: "--force",
        short: None,
        value: None,
        help: "Resume even into a Worker whose Workspace is open",
    },
    Flag {
        long: "--json",
        short: None,
        value: None,
        help: "Print one machine-readable object per Ticket instead of prose",
    },
];

/// The top-level command list, grouped. Each entry names a command that
/// [`PAGES`] must document, which is what `every_top_level_command_has_a_page`
/// enforces.
const TOP_LEVEL: &[Group] = &[
    Group {
        title: None,
        entries: &[
            Entry {
                key: "add",
                args: "<name> [-r <rev>]",
                summary: "Create workspace and print path (stdout)",
            },
            Entry {
                key: "rm",
                args: "[--force] <name>",
                summary: "Remove workspace",
            },
            Entry {
                key: "list",
                args: "",
                summary: "List workspaces",
            },
            Entry {
                key: "clean",
                args: "[--pool <name>]",
                summary: "Remove non-default workspaces",
            },
            Entry {
                key: "root",
                args: "",
                summary: "Print repo root",
            },
            Entry {
                key: "where",
                args: "",
                summary: "Show repo and workspace paths",
            },
            Entry {
                key: "path",
                args: "<name>",
                summary: "Print workspace path",
            },
            Entry {
                key: "refresh",
                args: "",
                summary: "Rebuild workspace cache",
            },
            Entry {
                key: "sync",
                args: "",
                summary: "Re-sync this workspace onto trunk (fetch + move)",
            },
            Entry {
                key: "colocation",
                args: "<action>",
                summary: "Enable/disable Git colocation (enable|disable|status)",
            },
            Entry {
                key: "help",
                args: "[<command>...]",
                summary: "Show this command list, or one command's page",
            },
        ],
    },
    Group {
        title: Some("Pool"),
        entries: &[
            Entry {
                key: "pool",
                args: "[<N>]",
                summary: "List the Worker Pools, or set `default`'s size to <N>",
            },
            Entry {
                key: "pool create",
                args: "<name>",
                summary: "Declare a named pool in `[pools.<name>]`",
            },
            Entry {
                key: "pool destroy",
                args: "[--force] <name>",
                summary: "Remove a named pool, draining first (`default` is refused)",
            },
            Entry {
                key: "pool resize",
                args: "<N>",
                summary: "Set a pool's size (the explicit form of `pool <N>`)",
            },
            Entry {
                key: "pool list",
                args: "[--json]",
                summary: "Show every pool's status",
            },
            Entry {
                key: "pool rm",
                args: "<worker>",
                summary: "Remove a worker from the pool (must not be busy)",
            },
            Entry {
                key: "pool reset",
                args: "<worker>",
                summary: "Reset a worker to idle",
            },
            Entry {
                key: "pool profile",
                args: "<runtime>",
                summary: "Set runtime profile (--provider/--model)",
            },
        ],
    },
    Group {
        title: Some("Dispatch and sessions"),
        entries: &[
            Entry {
                key: "dispatch",
                args: "<TICKET>...",
                summary: "Dispatch Tickets (d; --text/--from-file/--provider/--model/--json)",
            },
            Entry {
                key: "resume",
                args: "[<TICKET>...]",
                summary: "Re-run the queue's unfinished steps (interrupted work)",
            },
            Entry {
                key: "wait",
                args: "<worker>... [--until C] [--timeout S]",
                summary: "Block until Workers finish; the exit code is the answer",
            },
            Entry {
                key: "send",
                args: "<worker> <prompt>",
                summary: "Send a Follow-up (s; --force)",
            },
            Entry {
                key: "review",
                args: "<worker>",
                summary: "Address PR review comments (rev)",
            },
            Entry {
                key: "logs",
                args: "<worker>",
                summary: "Follow a Worker log (log)",
            },
            Entry {
                key: "mount",
                args: "<worker>",
                summary: "Mount a Worker in kitty (m)",
            },
            Entry {
                key: "rebase",
                args: "<worker>",
                summary: "Rebase and push a Worker (rb)",
            },
            Entry {
                key: "absorb",
                args: "<worker>",
                summary: "Fold a Worker's edits into their base commits (ab)",
            },
            Entry {
                key: "open-pr",
                args: "<worker>",
                summary: "Open a Worker's Pull Request (pr)",
            },
            Entry {
                key: "resolve",
                args: "<workspace>",
                summary: "Resolve a Workspace's conflicted commits (jj resolve)",
            },
            Entry {
                key: "undo",
                args: "[<worker>] [--force]",
                summary: "Undo the last jjfx mutation on a Worker (list when omitted)",
            },
            Entry {
                key: "ticket list",
                args: "[--json] [--label <LABEL>] [--pool <name>]",
                summary: "List every Ticket and whether it is ready (read-only)",
            },
            Entry {
                key: "ticket reconcile",
                args: "",
                summary: "Resolve finished local Tickets without a Dispatch",
            },
        ],
    },
    Group {
        title: Some("Lifecycle"),
        entries: &[
            Entry {
                key: "hooks install",
                args: "",
                summary: "Install lifecycle integrations for coding agents",
            },
            Entry {
                key: "hooks status",
                args: "",
                summary: "Report lifecycle integration state",
            },
            Entry {
                key: "children",
                args: "[<session>] [--json]",
                summary: "List a session's child sessions and what each is doing",
            },
        ],
    },
    Group {
        title: Some("Completion"),
        entries: &[Entry {
            key: "completion",
            args: "[zsh]",
            summary: "Print zsh completion",
        }],
    },
    Group {
        title: Some("Checks"),
        entries: &[Entry {
            key: "check migrations",
            args: "",
            summary: "Flag migration-order inversions across workspaces",
        }],
    },
    Group {
        title: Some("Observability"),
        entries: &[
            Entry {
                key: "status",
                args: "[--json]",
                summary: "Alias for pool list",
            },
            Entry {
                key: "version",
                args: "",
                summary: concat!("Print the ", env!("CARGO_PKG_NAME"), " version"),
            },
        ],
    },
];

/// One help page per command the top-level list names.
const PAGES: &[CommandHelp] = &[
    CommandHelp {
        key: "add",
        summary: "Create a workspace and print its path on stdout",
        usage: "add <name> [-r <rev>]",
        arguments: &[Argument {
            name: "<name>",
            help: "Name for the new workspace",
        }],
        flags: ADD_FLAGS,
    },
    CommandHelp {
        key: "rm",
        summary: "Remove one or more workspaces",
        usage: "rm [--force] <name> [name...]",
        arguments: &[Argument {
            name: "<name>",
            help: "Workspace to remove; repeat to remove several",
        }],
        flags: RM_FLAGS,
    },
    CommandHelp {
        key: "list",
        summary: "List workspaces",
        usage: "list",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "clean",
        summary: "Remove non-default workspaces, whole-repo or one pool",
        usage: "clean [--pool <name>]",
        arguments: &[],
        flags: CLEAN_FLAGS,
    },
    CommandHelp {
        key: "root",
        summary: "Print the repository root",
        usage: "root",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "where",
        summary: "Show the repository and workspace directories",
        usage: "where",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "path",
        summary: "Print a workspace's directory",
        usage: "path <name>",
        arguments: &[Argument {
            name: "<name>",
            help: "Workspace name, or a short Worker id",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "refresh",
        summary: "Rebuild the workspace cache",
        usage: "refresh",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "sync",
        summary: "Fetch and move the current workspace's working copy onto trunk",
        usage: "sync",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "colocation",
        summary: "Enable or disable Git colocation through jj",
        usage: "colocation <enable|disable|status>",
        arguments: &[Argument {
            name: "<enable|disable|status>",
            help: "Transition to perform",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "pool",
        summary: "Manage the Repository's Worker Pools",
        usage: "pool [<N>]",
        arguments: &[Argument {
            name: "<N>",
            help: "Optional: resize `default` to N Workers (safe shrink)",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "pool create",
        summary: "Declare a named Worker Pool in the repository config",
        usage: "pool create <name> [--size N] [--profile NAME] [--source SOURCE] [--label LABEL] [--ready-status STATUS] [--cap-usd USD] [--window WINDOW] [--on-exceed MODE]",
        arguments: &[Argument {
            name: "<name>",
            help: "The pool's name; it becomes the `[pools.<name>]` table key",
        }],
        flags: POOL_CREATE_FLAGS,
    },
    CommandHelp {
        key: "pool destroy",
        summary: "Remove a named pool, draining first; `default` is refused",
        usage: "pool destroy [--force] <name>",
        arguments: &[Argument {
            name: "<name>",
            help: "The pool to remove; `default` is never removable",
        }],
        flags: POOL_DESTROY_FLAGS,
    },
    CommandHelp {
        key: "pool resize",
        summary: "Set a pool's size, creating `default` when it does not exist",
        usage: "pool resize <N> [--pool NAME]",
        arguments: &[Argument {
            name: "<N>",
            help: "The size to grow or shrink the pool to; `pool <N>` is the short form",
        }],
        flags: POOL_RESIZE_FLAGS,
    },
    CommandHelp {
        key: "pool list",
        summary: "Show every pool, and each pool's Workers",
        usage: "pool list [--json]",
        arguments: &[],
        flags: POOL_LIST_FLAGS,
    },
    CommandHelp {
        key: "pool rm",
        summary: "Remove one Worker from the pool",
        usage: "pool rm <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker to remove; it must not be busy",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "pool reset",
        summary: "Reset one Worker to idle",
        usage: "pool reset <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker to reset to idle",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "pool profile",
        summary: "Set the runtime profile every Worker runs",
        usage: "pool profile <claude|codex|opencode> [--provider PROVIDER] [--model MODEL]",
        arguments: &[Argument {
            name: "<claude|codex|opencode>",
            help: "Agent runtime every Worker runs",
        }],
        flags: POOL_PROFILE_FLAGS,
    },
    CommandHelp {
        key: "dispatch",
        summary: "Dispatch Tickets to idle Workers",
        usage: "dispatch <TICKET>... [--fg|--bg] [--all] [--text TEXT | --from-file PATH | -] [--id ID] [--label LABEL] [--pool NAME] [--provider PROVIDER] [--model MODEL] [--budget USD] [--no-orchestrate] [--force] [--json]",
        arguments: &[
            Argument {
                name: "<TICKET>...",
                help: "Ticket ids to dispatch; use --all to discover them instead",
            },
            Argument {
                name: "-",
                help: "Read the Ticket from stdin; equivalent to --from-file -",
            },
        ],
        flags: DISPATCH_FLAGS,
    },
    CommandHelp {
        key: "resume",
        summary: "Re-run the work the durable queue records as unfinished",
        usage: "resume [<TICKET>...] [--fg|--bg] [--pool NAME] [--force] [--json]",
        arguments: &[Argument {
            name: "[<TICKET>...]",
            help: "Tickets to resume; omit to resume every unfinished step the queue holds",
        }],
        flags: RESUME_FLAGS,
    },
    CommandHelp {
        key: "wait",
        summary: "Block until Workers reach a condition, with the answer in the exit code",
        usage: "wait <worker>... [--until done|failed|terminal|ticket-resolved] [--timeout SECS] [--json] [--all] [--pool <name>]",
        arguments: &[Argument {
            name: "<worker>...",
            help: "Workers to wait for; use --all for every Worker in the pool",
        }],
        flags: WAIT_FLAGS,
    },
    CommandHelp {
        key: "send",
        summary: "Send a Follow-up prompt to a Worker",
        usage: "send <worker> <prompt> [--fg|--bg] [--force]",
        arguments: &[
            Argument {
                name: "<worker>",
                help: "Worker to send to",
            },
            Argument {
                name: "<prompt>",
                help: "Follow-up prompt",
            },
        ],
        flags: SEND_FLAGS,
    },
    CommandHelp {
        key: "review",
        summary: "Address a Worker's pull-request review comments",
        usage: "review <worker> [--fg|--bg]",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker whose review comments to address",
        }],
        flags: REVIEW_FLAGS,
    },
    CommandHelp {
        key: "logs",
        summary: "Follow a Worker's Run log",
        usage: "logs <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker whose log to follow",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "mount",
        summary: "Mount a Worker in a kitty tab",
        usage: "mount <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker to mount",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "rebase",
        summary: "Rebase and push a Worker's bookmark",
        usage: "rebase <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker to rebase and push",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "absorb",
        summary: "Fold a Worker's working-copy edits into their base commits",
        usage: "absorb <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker whose edits to fold in",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "open-pr",
        summary: "Open a Worker's Pull Request in the browser",
        usage: "open-pr <worker>",
        arguments: &[Argument {
            name: "<worker>",
            help: "Worker whose Pull Request to open",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "resolve",
        summary: "Resolve a Workspace's conflicted commits with jj resolve",
        usage: "resolve <workspace> [--tool NAME]",
        arguments: &[Argument {
            name: "<workspace>",
            help: "Workspace, or a short Worker id",
        }],
        flags: RESOLVE_FLAGS,
    },
    CommandHelp {
        key: "undo",
        summary: "Undo the last jjfx mutation on a Workspace",
        usage: "undo [<workspace>] [--force]",
        arguments: &[Argument {
            name: "[<workspace>]",
            help: "Worker id, workspace name, or `repo`/`*` for a repository-wide mutation; omit to list what can be undone",
        }],
        flags: UNDO_FLAGS,
    },
    CommandHelp {
        key: "ticket reconcile",
        summary: "Resolve finished local Tickets from the durable Run Record store",
        usage: "ticket reconcile",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "ticket list",
        summary: "List every Ticket the configured source discovers, and whether it is ready",
        usage: "ticket list [--json] [--label <LABEL>] [--pool <name>]",
        arguments: &[],
        flags: TICKET_LIST_FLAGS,
    },
    CommandHelp {
        key: "hooks install",
        summary: "Install the lifecycle integrations for Claude Code, Codex and OpenCode",
        usage: "hooks install",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "hooks status",
        summary: "Report each agent's lifecycle integration state",
        usage: "hooks status",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "children",
        summary: "List the child sessions a session dispatched, and what each is doing",
        usage: "children [<session>] [--json]",
        arguments: &[Argument {
            name: "[<session>]",
            help: "The parent session; defaults to the session that ran this command (OPENCODE_SESSION_ID)",
        }],
        flags: CHILDREN_FLAGS,
    },
    CommandHelp {
        key: "completion",
        summary: "Print the zsh completion script",
        usage: "completion [zsh]",
        arguments: &[Argument {
            name: "[zsh]",
            help: "Shell to complete for; zsh is the only supported shell",
        }],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "check migrations",
        summary: "Flag migration-order inversions across the workspaces",
        usage: "check migrations",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "status",
        summary: "Show pool status (alias for pool list)",
        usage: "status [--json]",
        arguments: &[],
        flags: STATUS_FLAGS,
    },
    CommandHelp {
        key: "version",
        summary: concat!("Print the ", env!("CARGO_PKG_NAME"), " version"),
        usage: "version",
        arguments: &[],
        flags: NO_FLAGS,
    },
    CommandHelp {
        key: "help",
        summary: "Show the command list, or one command's page",
        usage: "help [<command>...]",
        arguments: &[Argument {
            name: "[<command>...]",
            help: "Command to describe; omit it for the full command list",
        }],
        flags: NO_FLAGS,
    },
];

/// Spellings the parser accepts that the top-level list does not name, mapped
/// to the page that documents them.
///
/// The parser accepts more words than [`TOP_LEVEL`] lists - short aliases
/// (`d`, `rb`), the `reset` shorthand, and pool synonyms (`pool c`, `pool r`) -
/// so `jjfx help <spelling>` must answer for each. A word absent here, absent
/// from [`PAGES`], and heading no group is an unknown command, which is the
/// error a typo gets.
///
/// `every_parser_alias_resolves_to_its_page` holds this table to `text`, and
/// `cli::tests::the_parser_accepts_every_documented_alias` holds it to the
/// parser, so an accepted spelling cannot drift away from its page.
///
/// The hidden words (`__complete`, `__orchestrate`, the foreground wrapper) are
/// deliberately absent: they are private, and the top-level list keeps them so.
pub(crate) const ALIASES: &[(&str, &str)] = &[
    ("a", "add"),
    ("remove", "rm"),
    ("ls", "list"),
    ("info", "where"),
    ("colocate", "colocation"),
    ("reset", "pool reset"),
    ("d", "dispatch"),
    ("s", "send"),
    ("rev", "review"),
    ("log", "logs"),
    ("m", "mount"),
    ("rb", "rebase"),
    ("ab", "absorb"),
    ("pr", "open-pr"),
    ("-h", "help"),
    ("--help", "help"),
    ("--version", "version"),
    ("pool c", "pool create"),
    ("pool r", "pool resize"),
    ("pool ls", "pool list"),
    ("pool status", "pool list"),
    ("pool remove", "pool rm"),
    ("pool help", "pool"),
    ("pool -h", "pool"),
    ("pool --help", "pool"),
];

/// The page for `key`, or `None` when no command by that name exists.
fn page(key: &str) -> Option<&'static CommandHelp> {
    PAGES.iter().find(|page| page.key == key)
}

/// The page key a spelling resolves to: the spelling itself for a canonical
/// command, or the canonical key for a spelling the parser accepts as an alias.
fn canonical(key: &str) -> &str {
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or(key, |(_, canonical)| *canonical)
}

/// The top-level entries that name a subcommand of `key`: those whose key is
/// `key` followed by another word. This is what makes `ticket`, `hooks` and
/// `check` groups, and `pool` both a command and a group.
///
/// Membership is read from [`TOP_LEVEL`], the same table the top-level list
/// renders, so a group cannot exist in the list and not in `help <group>`:
/// adding `ticket foo` to the list gives `help ticket` its page with no further
/// edit.
fn group_entries(key: &str) -> Vec<&'static Entry> {
    TOP_LEVEL
        .iter()
        .flat_map(|group| group.entries.iter())
        .filter(|entry| {
            entry
                .key
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(' '))
        })
        .collect()
}

/// The page for a group that is not itself a command (`ticket`, `hooks`,
/// `check`): a heading, a usage line naming its subcommands, and their rows
/// follow from [`group_commands`]. `None` when `key` heads no group, or heads
/// one - `pool` - that already has a page of its own.
fn group_page(key: &str) -> Option<String> {
    let entries = group_entries(key);
    if entries.is_empty() || page(key).is_some() {
        return None;
    }
    let subcommands = entries
        .iter()
        .filter_map(|entry| entry.key.strip_prefix(key))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("|");
    Some(format!(
        "{PROGRAM} {key} - the `{key}` subcommands\n\nUsage: {PROGRAM} {key} <{subcommands}>\n"
    ))
}

/// The `Commands:` block a group page ends with, one line per subcommand, or
/// `None` when `key` heads no group. Appended to a group's own page too, so
/// `help pool` lists what `pool` can do rather than only `pool <N>`.
fn group_commands(key: &str) -> Option<String> {
    let entries = group_entries(key);
    if entries.is_empty() {
        return None;
    }
    let mut out = String::from("\nCommands:\n");
    for entry in entries {
        out.push_str(&command_line(entry));
    }
    Some(out)
}

/// One line of a command list: the command's words and arguments, padded, then
/// its summary. Shared by the top-level list and a group page, so a command is
/// spelled the same in both.
fn command_line(entry: &Entry) -> String {
    let field = if entry.args.is_empty() {
        entry.key.to_owned()
    } else {
        format!("{} {}", entry.key, entry.args)
    };
    format!("  {PROGRAM} {field:<25} {}\n", entry.summary)
}

/// The rendered help text: a command's page when `key` names one, a group's
/// subcommands when it names a group (`ticket`, `hooks`), otherwise the
/// top-level command list for a bare `help`. An unknown command is a loud error
/// naming it, so a typo never silently prints the wrong thing.
pub(crate) fn text(key: Option<&str>) -> Result<String> {
    let Some(key) = key else {
        return Ok(top_level());
    };
    let key = canonical(key);
    let mut out = match page(key) {
        Some(page) => page.render(),
        None => group_page(key).ok_or_else(|| {
            anyhow::anyhow!("unknown command: {key}. Run '{PROGRAM} help' for the command list.")
        })?,
    };
    if let Some(commands) = group_commands(key) {
        out.push_str(&commands);
    }
    Ok(out)
}

/// The rendered top-level command list.
pub(crate) fn top_level() -> String {
    let mut out =
        format!("{PROGRAM} - jj workspace manager\n\nUsage: {PROGRAM} [OPTIONS]\n\nUsage:\n");
    for group in TOP_LEVEL {
        if let Some(title) = group.title {
            out.push_str(&format!("\n{title}:\n"));
        }
        for entry in group.entries {
            out.push_str(&command_line(entry));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cli::parse;

    /// A command in the top-level list without a page is a dead end for a
    /// reader: `jjfx help <command>` would fail. Adding a command to the list
    /// must therefore add its page in the same change. Driving `text` - the
    /// function the `help` command calls - covers the lookup and the render.
    #[test]
    fn every_top_level_command_has_a_page() {
        for group in TOP_LEVEL {
            for entry in group.entries {
                let rendered = text(Some(entry.key)).unwrap_or_else(|error| {
                    panic!(
                        "top-level command `{}` has no help page: {error}",
                        entry.key
                    )
                });
                assert!(
                    rendered.contains(&format!("Usage: {PROGRAM} ")),
                    "page `{}` has no usage line",
                    entry.key
                );
            }
        }
    }

    /// The parser accepts more spellings than the top-level list names - short
    /// aliases and pool synonyms. `jjfx help <spelling>` must answer for each
    /// of them, so an accepted word never dead-ends the way `pool create` once
    /// did.
    #[test]
    fn every_parser_alias_resolves_to_its_page() {
        for (alias, key) in ALIASES {
            let rendered = text(Some(alias))
                .unwrap_or_else(|error| panic!("alias `{alias}` has no help page: {error}"));
            let expected = text(Some(key)).unwrap_or_else(|error| {
                panic!("alias `{alias}` names a missing page `{key}`: {error}")
            });
            assert_eq!(
                rendered, expected,
                "alias `{alias}` did not resolve to the `{key}` page"
            );
        }
    }

    /// The reverse: a page for a command the top-level list does not name is
    /// unreachable - nothing tells a reader it exists.
    #[test]
    fn every_page_is_named_at_the_top_level() {
        let listed: Vec<&str> = TOP_LEVEL
            .iter()
            .flat_map(|group| group.entries.iter().map(|entry| entry.key))
            .collect();
        for page in PAGES {
            assert!(
                listed.contains(&page.key),
                "help page `{}` is not in the top-level list",
                page.key
            );
        }
    }

    /// Each page must actually print what it promises: a usage line, and every
    /// argument and flag it declares.
    #[test]
    fn every_page_renders_its_usage_arguments_and_flags() {
        for page in PAGES {
            let rendered = page.render();
            assert!(
                rendered.contains(&format!("Usage: {PROGRAM} {}", page.usage)),
                "page `{}` is missing its usage line",
                page.key
            );
            for argument in page.arguments {
                assert!(
                    rendered.contains(argument.name),
                    "page `{}` omits argument `{}`",
                    page.key,
                    argument.name
                );
            }
            for flag in page.flags {
                assert!(
                    rendered.contains(flag.long),
                    "page `{}` omits flag `{}`",
                    page.key,
                    flag.long
                );
                if let Some(short) = flag.short {
                    assert!(
                        rendered.contains(short),
                        "page `{}` omits short flag `{short}`",
                        page.key
                    );
                }
                if let Some(value) = flag.value {
                    assert!(
                        rendered.contains(&format!("<{value}>")),
                        "page `{}` omits the `{}` placeholder for {}",
                        page.key,
                        value,
                        flag.long
                    );
                }
            }
        }
    }

    /// The promise this module makes, made checkable: a command's page and its
    /// parser read the same flag definitions, so the set of flags the parser
    /// accepts for a command must equal the set its page documents. The parser
    /// is probed through its public `parse` - not by comparing the two slices,
    /// which would be tautological - so a `parse_*` that stops validating
    /// against the page's slice fails here, in either direction: a page flag the
    /// parser rejects, or a flag the parser accepts that no page documents.
    ///
    /// Scoped to the pages that declare flags. A command with none parses a bare
    /// positional and never inspects options, so `open-pr --json` is accepted as
    /// a Worker named `--json` rather than as a flag - there is no accepted-flag
    /// set to hold it to, and its surface is covered by
    /// `every_page_renders_its_usage_arguments_and_flags`.
    #[test]
    fn the_parser_accepts_exactly_the_flags_a_page_documents() {
        // Every spelling any page defines, with whether it takes a value: the
        // probe set every command is asked about.
        let mut probes: Vec<(&str, bool)> = Vec::new();
        for page in PAGES {
            for flag in page.flags {
                for spelling in [Some(flag.long), flag.short].into_iter().flatten() {
                    if !probes.iter().any(|(known, _)| *known == spelling) {
                        probes.push((spelling, flag.value.is_some()));
                    }
                }
            }
        }
        for page in PAGES.iter().filter(|page| !page.flags.is_empty()) {
            for (spelling, takes_value) in &probes {
                let documented = page.flags.iter().any(|flag| flag.matches(spelling));
                let mut args: Vec<String> = page.key.split(' ').map(str::to_owned).collect();
                args.push((*spelling).to_owned());
                if *takes_value {
                    args.push("value".to_owned());
                }
                let outcome = parse(&args);
                let rejected = outcome
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.to_string().contains("unknown option"));
                assert_eq!(
                    documented,
                    !rejected,
                    "`{} {}`: documented = {documented}, but the parser {} ({:?})",
                    page.key,
                    spelling,
                    if rejected {
                        "rejected it"
                    } else {
                        "did not reject it"
                    },
                    outcome.err().map(|error| error.to_string())
                );
            }
        }
    }

    #[test]
    fn an_unknown_command_is_named_in_the_error() {
        let error = text(Some("definitely-not-a-command"))
            .expect_err("an unknown command must fail loudly");
        assert!(error.to_string().contains("definitely-not-a-command"));
    }

    /// A group is a command word the top-level list only ever spells with a
    /// subcommand after it (`ticket list`). `jjfx help ticket` must answer with
    /// that group's subcommands rather than calling the group unknown - the
    /// obvious next probe after reading a two-word command in the list.
    #[test]
    fn a_multi_word_group_prints_its_subcommands() {
        let ticket = text(Some("ticket")).expect("`ticket` heads a group");
        assert!(ticket.contains("ticket list"), "{ticket}");
        assert!(ticket.contains("ticket reconcile"), "{ticket}");
        assert!(ticket.contains("Commands:"), "{ticket}");
        assert!(!ticket.contains("unknown command"), "{ticket}");
    }

    /// The other half of the group page: naming the subcommand still prints its
    /// own page, not the group's.
    #[test]
    fn a_group_subcommand_still_prints_its_page() {
        let list = text(Some("ticket list")).expect("ticket list has a page");
        assert!(list.contains("Usage: jjfx ticket list"), "{list}");
        assert!(!list.contains("Commands:"), "{list}");
    }

    /// Every group the top-level table spells must resolve, so adding a
    /// `foo bar` command to the list cannot leave `help foo` an unknown
    /// command. The check is derived from [`TOP_LEVEL`], so it covers `ticket`,
    /// `hooks` and `check` without naming them.
    #[test]
    fn every_group_head_resolves_and_names_its_commands() {
        for group in TOP_LEVEL {
            for entry in group.entries {
                let Some((head, _)) = entry.key.split_once(' ') else {
                    continue;
                };
                let rendered = text(Some(head))
                    .unwrap_or_else(|error| panic!("group `{head}` has no help page: {error}"));
                assert!(
                    rendered.contains(entry.key),
                    "group page `{head}` omits its command `{}`",
                    entry.key
                );
            }
        }
    }

    /// `pool` is both a command and a group: its own page must still render,
    /// and it must also list the subcommands a reader would otherwise have to
    /// find in the top-level list.
    #[test]
    fn a_page_that_heads_a_group_also_lists_its_commands() {
        let pool = text(Some("pool")).expect("pool has a page");
        assert!(
            pool.contains("Manage the Repository's Worker Pools"),
            "{pool}"
        );
        assert!(pool.contains("Commands:"), "{pool}");
        assert!(pool.contains("pool create"), "{pool}");
        assert!(pool.contains("pool profile"), "{pool}");
    }

    /// An unknown word is still a loud error, even when it looks like a group
    /// with a subcommand glued on.
    #[test]
    fn an_unknown_group_is_named_in_the_error() {
        let error = text(Some("ticket missing"))
            .expect_err("an unknown subcommand of a real group must fail loudly");
        assert!(error.to_string().contains("ticket missing"), "{error}");
    }

    #[test]
    fn the_top_level_help_lists_the_lifecycle_commands() {
        let help = top_level();
        assert!(help.contains("hooks install"), "{help}");
        assert!(help.contains("hooks status"), "{help}");
    }

    #[test]
    fn the_top_level_help_keeps_hidden_commands_private() {
        let help = top_level();
        assert!(!help.contains("__complete"), "{help}");
        assert!(!help.contains("__orchestrate"), "{help}");
        assert!(help.contains("completion [zsh]"), "{help}");
    }

    #[test]
    fn a_named_page_is_returned_and_a_bare_help_is_the_command_list() {
        let dispatch = text(Some("dispatch")).expect("dispatch has a page");
        assert!(dispatch.contains("Usage: jjfx dispatch"), "{dispatch}");
        assert!(dispatch.contains("--no-orchestrate"), "{dispatch}");
        assert_eq!(text(None).expect("top-level help"), top_level());
    }

    /// The ad-hoc Ticket inputs are part of the documented surface: a reader of
    /// `jjfx help dispatch` can find how to hand a Ticket in directly.
    #[test]
    fn the_dispatch_page_documents_the_ad_hoc_ticket_inputs() {
        let dispatch = text(Some("dispatch")).expect("dispatch has a page");
        for flag in ["--text", "--from-file", "--id"] {
            assert!(dispatch.contains(flag), "{dispatch}");
        }
        assert!(dispatch.contains("stdin"), "{dispatch}");
    }

    /// `wait` is a blocking command whose answer is the exit code, so its page
    /// must document both the conditions a caller waits on and the exit codes a
    /// caller switches on.
    #[test]
    fn the_wait_page_documents_conditions_and_exit_codes() {
        let wait = text(Some("wait")).expect("wait has a page");
        for condition in ["done", "failed", "terminal", "ticket-resolved"] {
            assert!(wait.contains(condition), "{wait}");
        }
        assert!(wait.contains("Exit codes:"), "{wait}");
        assert!(wait.contains("the condition was reached"), "{wait}");
        assert!(wait.contains("timed out"), "{wait}");
        assert!(wait.contains("does not satisfy --until"), "{wait}");
    }

    /// `ticket list` is the read-only backlog view, and the one thing a reader
    /// must not get wrong is that it changes nothing: its page says so, and says
    /// where its readiness comes from.
    #[test]
    fn the_ticket_list_page_documents_the_read_only_backlog() {
        let list = text(Some("ticket list")).expect("ticket list has a page");
        assert!(list.contains("--json"), "{list}");
        assert!(list.contains("--pool"), "{list}");
        assert!(list.contains("read-only"), "{list}");
        assert!(list.contains("reconciles no Run"), "{list}");
        assert!(list.contains("dispatch --all"), "{list}");
    }

    /// The dispatch page must say how `--fg` relates to `wait`: one blocks for
    /// this Dispatch's Runs, the other re-attaches to any Worker.
    #[test]
    fn the_dispatch_page_relates_fg_to_wait() {
        let dispatch = text(Some("dispatch")).expect("dispatch has a page");
        assert!(dispatch.contains("--fg"), "{dispatch}");
        assert!(dispatch.contains("wait"), "{dispatch}");
    }

    /// `sync` moves the working copy onto trunk, which is easy to confuse with
    /// `refresh` (rebuild the cache) and `rebase <worker>` (push a Worker), so
    /// its page must spell out how they differ.
    #[test]
    fn the_sync_page_distinguishes_refresh_and_worker_rebase() {
        let sync = text(Some("sync")).expect("sync has a page");
        assert!(sync.contains("refresh"), "{sync}");
        assert!(sync.contains("workspace cache"), "{sync}");
        assert!(sync.contains("rebase"), "{sync}");
        assert!(sync.contains("Worker"), "{sync}");
    }
}
