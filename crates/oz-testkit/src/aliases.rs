//! Curated alias catalogue, mirrored from `oz-core-types/src/tool_args.rs`.
//!
//! This is intentionally data, not a parser: a case generator that scrapes the
//! implementation would inherit its bugs. `oz-casegen --check` verifies every
//! alias still maps to the canonical key declared here, so drift is loud.

pub struct Group {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
}

pub struct ToolAliases {
    pub tool: &'static str,
    pub groups: &'static [Group],
}

const CODE: Group = Group {
    canonical: "code",
    aliases: &[
        "cmd",
        "command",
        "commandline",
        "shell",
        "script",
        "bash",
        "sh",
        "code_run",
        "coderun",
        "source",
        "content",
        "input",
        "snippet",
        "program",
        "python",
        "text",
    ],
};
const CODE_TYPE: Group = Group {
    canonical: "type",
    aliases: &[
        "mode",
        "lang",
        "language",
        "interpreter",
        "kind",
        "runtime",
        "codelang",
    ],
};
const CODE_TIMEOUT: Group = Group {
    canonical: "timeout",
    aliases: &[
        "timeoutsecs",
        "timeoutseconds",
        "maxtime",
        "maxseconds",
        "timeoutvalue",
    ],
};
/// `mode` stays a canonical key of its own; it is only re-keyed when its value
/// is a language keyword (pass 1 in `canonicalize_tool_args`).
const CODE_MODE: Group = Group {
    canonical: "mode",
    aliases: &[],
};

const FILE: Group = Group {
    canonical: "file_path",
    aliases: &[
        "path",
        "file",
        "filename",
        "filepath",
        "filepaths",
        "target",
        "source",
        "src",
        "dest",
        "destination",
        "absolutepath",
        "fullpath",
        "inputfile",
        "fileordir",
        "readpath",
        "writepath",
    ],
};
const CONTENT: Group = Group {
    canonical: "content",
    aliases: &[
        "text",
        "data",
        "body",
        "contents",
        "filecontent",
        "newcontent",
        "payload",
        "output",
        "value",
        "code",
        "script",
    ],
};
const OLD: Group = Group {
    canonical: "old_string",
    aliases: &[
        "old",
        "oldtext",
        "oldstr",
        "oldcontent",
        "find",
        "search",
        "before",
        "from",
        "original",
        "targettext",
        "oldvalue",
    ],
};
const NEW: Group = Group {
    canonical: "new_string",
    aliases: &[
        "new",
        "newtext",
        "newstr",
        "newcontent",
        "replace",
        "replacement",
        "replacementtext",
        "with",
        "after",
        "to",
        "newvalue",
    ],
};
const START: Group = Group {
    canonical: "start",
    aliases: &[
        "startline",
        "offset",
        "from",
        "begin",
        "firstline",
        "linestart",
    ],
};
const COUNT: Group = Group {
    canonical: "count",
    aliases: &[
        "lines",
        "limit",
        "numlines",
        "linecount",
        "maxlines",
        "length",
    ],
};
const PATTERN: Group = Group {
    canonical: "pattern",
    aliases: &[
        "regex",
        "regexp",
        "query",
        "match",
        "globpattern",
        "filepattern",
    ],
};
const DIR: Group = Group {
    canonical: "path",
    aliases: &[
        "directory",
        "dir",
        "base",
        "basepath",
        "root",
        "folder",
        "searchpath",
        "cwd",
    ],
};
const INCLUDE: Group = Group {
    canonical: "include",
    aliases: &[
        "glob",
        "filter",
        "fileglob",
        "includepattern",
        "include_pattern",
    ],
};
const QUESTION: Group = Group {
    canonical: "question",
    aliases: &["q", "prompt", "query", "text", "message", "ask"],
};
const CANDIDATES: Group = Group {
    canonical: "candidates",
    aliases: &["options", "choices", "suggestions", "answers"],
};
const ARTIFACT_PATH: Group = Group {
    canonical: "artifact_path",
    aliases: &[
        "path",
        "file",
        "filepath",
        "filename",
        "artifactpath",
        "artifact",
        "target",
    ],
};
const ARTIFACT_TYPE: Group = Group {
    canonical: "artifact_type",
    aliases: &["type", "kind", "filetype", "format", "artifacttype"],
};
const ARTIFACT_LABEL: Group = Group {
    canonical: "artifact_label",
    aliases: &["label", "title", "name", "tab"],
};

macro_rules! tool {
    ($tool:expr, [$($group:expr),+ $(,)?]) => {
        ToolAliases { tool: $tool, groups: &[$($group),+] }
    };
}

/// Tools with a curated table. `ls` is deliberately absent: its primary key is
/// `path` while the path-family cross-fill also populates `file_path`, so an
/// exact-output oracle would encode an implementation quirk instead of a
/// contract. It is covered by the contract/type checks instead.
pub const TOOL_ALIASES: &[ToolAliases] = &[
    tool!("code_run", [CODE, CODE_TYPE, CODE_TIMEOUT, CODE_MODE]),
    tool!("read", [FILE, START, COUNT]),
    tool!("write", [FILE, CONTENT]),
    tool!("edit", [FILE, OLD, NEW]),
    tool!("patch", [FILE, OLD, NEW]),
    tool!("glob", [PATTERN, DIR]),
    tool!("grep", [PATTERN, DIR, INCLUDE]),
    tool!("ask_user", [QUESTION, CANDIDATES]),
    tool!(
        "open_side_panel",
        [ARTIFACT_PATH, ARTIFACT_TYPE, ARTIFACT_LABEL]
    ),
];

pub fn groups_for(tool: &str) -> Option<&'static [Group]> {
    TOOL_ALIASES
        .iter()
        .find(|t| t.tool == tool)
        .map(|t| t.groups)
}

pub fn canonical_names(tool: &str) -> Vec<&'static str> {
    groups_for(tool)
        .map(|g| g.iter().map(|x| x.canonical).collect())
        .unwrap_or_default()
}
