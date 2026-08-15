# Tool Registry

The tool registry manages all tools available to the agent during a
conversation. It tracks where each tool comes from and supports filtering by
source.

## Tool Sources

Every registered tool has a `ToolSource` that identifies its origin:

- **`Builtin`** — tools shipped with the binary (`execute_command`, `read_file`,
  etc.)
- **`Mcp { server }`** — tools provided by an MCP server, tagged with the server
  name

This replaces the previous convention of identifying MCP tools by their `mcp__`
name prefix, providing type-safe filtering instead of string matching.

## Registration

```rust
// Built-in tool
registry.register(Box::new(MyTool::new()));

// MCP tool — tagged with server name
registry.register_mcp(Box::new(adapter), "github".to_string());
```

## Filtering

When MCP tools are disabled for a session, the registry can produce a filtered
copy:

```rust
// Type-safe: filters by ToolSource::Mcp variant
let no_mcp = registry.clone_without_mcp();

// Remove all MCP tools in-place (used during sync)
let removed_count = registry.unregister_mcp();
```

## Schema Output

`list_schemas()` includes source metadata in every tool schema:

```json
{
  "name": "execute_command",
  "description": "Execute a command",
  "parameters": { ... },
  "source": "builtin"
}
```

```json
{
  "name": "mcp__github__search",
  "description": "Search GitHub",
  "parameters": { ... },
  "source": "mcp",
  "mcpServer": "github"
}
```

The `source` and `mcpServer` fields are available to the UI for rendering tools
grouped by origin.

## Command execution tools

Chelix registers command execution tools for agent command work:

- `execute_command` runs through the session's managed `chelix-tools-service`
  route: the host sidecar when sandboxing is disabled or the service inside the
  selected sandbox when sandboxing is enabled.
- `read_terminal_output` reads retained output from the same managed terminal
  pool by `terminalId`. Use it after a foreground timeout or for a background
  command that continues after `execute_command` returns.

`execute_command` requires only `command`. The optional parameters are:

- `customCwd`: working directory for the command;
- `newTerminal`: create a new persistent terminal instead of reusing one;
- `destructiveFlag`: approval UI hint;
- `background`: return immediately after starting the command;
- `timeout`: milliseconds to wait for completion without terminating the
  process;
- `terminalId`: run in the terminal returned by an earlier call.

Omit optional routing parameters when they do not apply. Empty `customCwd` and
`terminalId` strings are treated as omitted. A non-empty `terminalId` cannot be
combined with `newTerminal = true`. Invalid terminal IDs and invalid working
directories are returned as explicit tool errors.

Each terminal is an in-process RMUX PTY with one persistent interactive shell.
Its current directory, exported variables, shell functions, job-control state,
child processes, terminal emulator state, and retained output remain associated
with the returned numeric `terminalId`. Reusing that ID continues in the same
shell; creating a new terminal starts a separate shell with the environment
that is current at creation time.

## Managed filesystem tools

The `edit_file`, `multiedit_file`, `read_file`, `read_media`, `list_directory`,
`overwrite_file`, and `ripgrep` tools execute exclusively through the managed
`chelix-tools-service`. With sandbox mode enabled, the service runs in the
sandbox container selected for the session. With sandbox mode disabled, Chelix
starts the service as a host sidecar. Service and filesystem errors are returned
to the tool caller; these tools never fall back from the sandbox to the gateway
host.

### `edit_file`

`edit_file` requires an absolute `filePath` and a nested `edit` object. The
unique form contains required `oldString` and `newString` fields. The explicit
form additionally requires boolean `replaceAll`. Both forms are strict
`oneOf` branches with no additional fields; no root-level default is applied.
The tool edits an existing regular UTF-8 file and follows symbolic links.

```json
{
  "filePath": "/workspace/file.txt",
  "edit": {
    "oldString": "old",
    "newString": "new"
  }
}
```

```json
{
  "filePath": "/workspace/file.txt",
  "edit": {
    "oldString": "old",
    "newString": "new",
    "replaceAll": true
  }
}
```

When a literal match is absent, LF input can match CRLF file content and
straight quotes can match Unicode smart quotes. The structured result reports
`filePath`, the number of `replacements`, the applied `replaceAll` value, and
an optional `recovery` value of `crlf` or `smart_quotes`.

Calls that resolve to the same target are serialized in the service. The
complete edit is prepared before the existing file is written in place, which
preserves its inode and permissions. Symbolic links are followed and preserved.
Relative paths, unknown or invalid parameters, missing or non-UTF-8 files,
non-unique matches, and absent matches are explicit errors that leave the file
unchanged. Persistence failures are explicit errors, but an I/O failure or
process interruption after writing starts can leave partially updated content.

### `multiedit_file`

`multiedit_file` requires an absolute `filePath` and a non-empty ordered
`edits` array. Each item uses one of the strict `edit_file` operation forms:
the unique form requires `oldString` and `newString`; the replace-all form
additionally requires boolean `replaceAll`.

```json
{
  "filePath": "/workspace/file.txt",
  "edits": [
    {
      "oldString": "old",
      "newString": "intermediate"
    },
    {
      "oldString": "intermediate",
      "newString": "new",
      "replaceAll": true
    }
  ]
}
```

Edits run sequentially against one in-memory buffer, so each item sees all
preceding results. The service begins persistence only after the complete batch
succeeds; any edit failure reports the one-based edit index and leaves the file
unchanged. The structured response reports `filePath`, `editsApplied`,
`replacementsPerEdit`, and ordered `recoveriesPerEdit` entries (`null`, `crlf`,
or `smart_quotes`).

The service serializes all write tools that resolve to the same target and
writes successful results in place, preserving the inode and permissions.
Symbolic links are followed and preserved. Invalid parameters, relative paths,
missing or non-regular files, non-UTF-8 content, and match failures are explicit
errors. Persistence failures are also explicit, but an I/O failure or process
interruption after writing starts can leave partially updated content.

### `read_file`

`read_file` requires an absolute `filePath` and a nested `read` object.
`includeLineNumbers` and `numberBlankLines` are root-level options for every
text read. An offset/limit read has required `offset` and `limit` fields.
Positive offsets are 1-indexed, and `offset = -1` selects tail mode. `limit = -1`
reads to the end of the file without the bounded-read line cap. A range
read has a non-empty `ranges` array of inclusive text line ranges, where the
optional `endLine` accepts `-1` to read to the last line of the file, and can
include range headers.

A text offset/limit read with an explicit positive limit returns at most 2,000
lines. A positive-offset read whose requested limit exceeds that cap includes
a continuation message. A read with `limit = -1` is not capped and carries no
continuation message. Binary files return a hexadecimal dump, use positive
offsets as 1-indexed byte positions, return at most 512 bytes, and reject
`limit = -1`. Empty and whitespace-only text files return explicit messages.
Invalid parameters, missing files, directories, unreadable files, and relative
paths are tool errors.

### `read_media`

`read_media` requires an absolute `filePath` and handles PDF documents plus image
files through the managed service. PDF-specific options live under an optional
`pdf` object; `pdf.pages` accepts either a single 1-indexed page like `3` or an
inclusive page range like `10-20`. Omit `pdf` entirely for images.

Images are optimized through `chelix_media::image_ops::optimize_for_llm()` and
returned with MIME type, dimensions, resize metadata, byte size, and a base64
payload. PDFs are decoded through `pdf-extract` and return extracted text plus
page metadata (`totalPages`, `pagesReturned`, `startPage`, `endPage`,
`truncated`). Media decode failures are explicit tool errors.

### `overwrite_file`

`overwrite_file` requires an absolute `filePath` and the complete UTF-8
`content`. It creates a new file or writes an existing regular file in place.
Parent directories must already exist. An empty `content` truncates the target.
Symbolic links are followed and preserved; a dangling link creates its target
when the target parent exists. Non-regular targets are rejected. Existing
targets retain their inode and permissions. An I/O failure or process
interruption after writing starts can leave partially updated content. The
result reports the resolved `filePath` and UTF-8 `bytesWritten`.

### `list_directory`

`list_directory` accepts one required absolute `path` and lists only its direct
children. The plain-text result uses the following format:

- directories end in `/`;
- text files include their logical line count, for example
  `notes.txt (2 lines)`;
- binary files include a binary marker and byte-based size, for example
  `image.png (binary, 12.4 KB)`;
- an empty directory returns `Folder is empty`.

A missing, relative, non-directory, or unreadable path is a tool error. Access
is limited by the filesystem visible to the managed service runtime.

## Ripgrep tool

The `ripgrep` tool searches files by shelling out to the system `rg` binary
with `--json` output and returns structured results. The binary is assumed to
be installed — a spawn failure surfaces as a tool error.

Parameters (camelCase): `pattern` (required), `paths`, `cwd`, `fixedStrings`,
`multiline`, `caseMode` (`sensitive`/`ignore`/`smart`), `detail` (`summary`, `files`,
`lines` — default, `lines+submatches`), `glob`, `type`, `typeNot`,
`contextLines`, `maxMatches` (300), `maxFiles` (100), `maxOutputChars`
(30000), `timeoutMs` (30000), `includeHidden` (default `true`),
`unrestricted` (0–3, default 3, maps to `-u`/`-uu`/`-uuu`), `gitignore`
(default `true`), `followSymlinks`.

`multiline` defaults to `false`. When enabled, it maps to `-U/--multiline` and
allows matches to span line terminators. It does not make `.` match line
terminators; dot-all behavior must be requested explicitly in the pattern.

`gitignore` switches every git-sourced rg filter as one unit: `.gitignore`
files, `.git/info/exclude`, and `core.excludesFile`. When enabled, the rules
also apply from parent directories and outside a git repository. Paths listed
explicitly in `paths` are searched by rg regardless of these rules.

Common extension-like `type` values (`tsx`, `jsx`, `mjs`, …) are normalized to
rg type names; unknown extension-like values become glob filters; anything
else is passed to rg verbatim so rg itself rejects unknown types.

Exceeding a match/file/output limit or the timeout stops the search early,
kills the rg process, and marks the result `truncated` with a
`truncatedReason` (`maxMatches`, `maxFiles`, `maxOutputChars`, `timeout`).
The result mirrors the limits, a summary (`filesWithMatches`, `matchCount`,
`elapsed`, `stats`), rows per detail mode, captured `stderr`, and the rg
`exitCode`. Exit code 2 (for example an invalid regex) is a tool error.

## GitHub tools

The `github_*` tools call the GitHub REST API through one shared client. When
configured, the client reads its personal access token from `tools.github.pat`:

```toml
[tools.github]
pat = "ghp_..."
request_timeout_secs = 300
```

The tools are always registered. Code search and file-content reads require a
configured token before issuing a request. Repository and issue search, directory
listing, release and issue listing, latest-release and issue reads, pull-request
listing, and pull-request reads can access public data without a token; if GitHub
denies an unauthenticated request with `401`/`403`, the call returns the explicit
missing-token error. A `401`/`403` response received with a configured token is an
authorization error rather than a re-authentication prompt.

Every request sends `X-GitHub-Api-Version: 2022-11-28` and uses
`Accept: application/vnd.github.v3+json` unless an endpoint requires a
specialised media type. Every request has the finite HTTP deadline configured
by `tools.github.request_timeout_secs` (default `300`, minimum `1`). A timeout
returns `GitHub request timed out after <duration>`. A `403`/`429` response that carries
`retry-after`, `x-ratelimit-remaining: 0`, or a body mentioning a rate limit is
treated as rate limited. When such a response provides usable timing
(`retry-after`, or `x-ratelimit-reset` with `x-ratelimit-remaining: 0`), the
shared client blocks every GitHub call across concurrent sessions until that
cooldown plus a 5-second buffer expires. One waiting call is then admitted as a
probe; the remaining calls wait for its outcome. A limited call retries once
through this shared gate. Without usable timing the response is returned as an
error. Cooldown start or extension, cooldown waits, probe waits, probe admission,
probe release, and probe termination without a response are emitted to tracing.
The per-request deadline also bounds a probe; timeout or cancellation releases
its lease and reopens the gate.

### `github_search_code`

Searches code via `GET /search/code`. `query` is required; the optional
integer `perPage` selects the page size between 1 and 100. The result is
markdown-formatted text: a
`GitHub Code Search Results (showing <n> of <total>)` header followed by
`Repo`, `File`, `Name`, `SHA`, and `URL` lines per item separated by
`----------`. An empty result set returns
`No code results found for this query.`. API failures are reported as
`GitHub code search API error: <message>`.

### `github_search_repositories`

Searches repositories via `GET /search/repositories`. `query` is required; the
optional integer `perPage` selects the page size between 1 and 100. The result
is markdown-formatted text: a
`GitHub Repository Search Results (showing <n> of <total>)` header followed by
`Name`, optional `Description`, `Stars`, `Forks`, optional `Language`, and `URL`
lines per repository separated by `----------`. An empty result set returns
`No repositories found for this query.`. A non-successful API response returns
the GitHub response body, or the HTTP status line when the body is empty.

### `github_search_issues`

Searches issues via `GET /search/issues`. `query` is required; the optional integer
`perPage` selects the page size between 1 and 100. The tool adds `is:issue` when the
query does not already contain that qualifier and excludes any unexpected result
containing the GitHub `pull_request` marker. The result starts with
`GitHub Issue Search Results (showing <n> of <total>)`. Each issue contains optional
`Repo`, then `Number`, `Title`, `State`, optional `Author`, `Comments`, `Created`,
`Updated`, optional `Labels`, and `URL`. Entries are separated by `----------`. An
empty result returns `No issues found for this query.`. A non-successful response
returns the GitHub response body, or the HTTP status line when the body is empty.

### `github_get_file_contents`

Reads one file via `GET /repos/{owner}/{repo}/contents/{path}`. `owner`,
`repo`, and `path` are required; the optional `ref` selects a
commit/branch/tag. The result is a markdown header (`# <name>`, `Repository`,
`Path`, optional `Ref`, `Size`, `SHA`, `URL`) followed by the decoded file
content in a `~~~` fenced block. A rate-limit response remaining after the
single controlled retry returns the GitHub response body. Any other response
that is not a readable file returns
`Failed to retrieve file content from GitHub (not found or unsupported type)`,
and a file without decodable content returns
`Unsupported or empty file content returned by GitHub API`.

### `github_get_directory_contents`

Lists a directory via `GET /repos/{owner}/{repo}/contents/{path}`. `owner` and
`repo` are required. Optional `path` selects a directory and accepts an empty
value for the repository root; optional `ref` selects a commit/branch/tag. The
result starts with `GitHub Directory Contents`, followed by `Repo`, normalized
`Path`, optional `Ref`, and `Entries` lines. Each entry contains `Name`, `Path`,
`Type`, optional `Size`, `SHA`, and `URL`, with entries separated by
`----------`. A nullable GitHub `html_url`, including an external submodule URL,
is rendered as `URL: null`. An empty directory ends with `(empty directory)`. A file path
returns
`The provided path points to a file. Use github_get_file_contents instead.`;
other non-directory and unexpected response shapes are explicit errors.

### `github_get_latest_release`

Retrieves the latest release via
`GET /repos/{owner}/{repo}/releases/latest`. `owner` and `repo` are required. The
result starts with `Latest GitHub Release for <owner>/<repo>`, followed by `Tag`,
optional `Name`, `Draft`, `Pre-release`, `Published`, and `URL`. A null published
date is rendered as `N/A`. A non-successful response returns the GitHub response
body, or the HTTP status line when the body is empty.

### `github_issue_read`

Reads one issue via `GET /repos/{owner}/{repo}/issues/{issue_number}`. `owner`,
`repo`, and the integer `issueNumber` are required. The result starts with
`GitHub Issue (full) <owner>/<repo> #<number>` and preserves the reference field
ordering for issue metadata, optional user, closer, milestone, labels, and reaction
fields. The final `Body` section contains the complete issue body or `(empty)` when
the body is absent or blank. A non-successful response returns the GitHub response
body, or the HTTP status line when the body is empty.

### `github_list_issues`

Lists open repository issues via `GET /search/issues` with the
`repo:<owner>/<repo> is:issue state:open` query, so GitHub excludes pull requests
and closed issues before applying pagination. The request uses `sort=created` and
`order=desc`. `owner` and
`repo` are required and accept only ASCII letters, digits, hyphens, underscores,
and periods; the optional integer `perPage` selects the issue page size between 1
and 100. The result starts with
`GitHub Issues for <owner>/<repo> (showing <n>)`. Each issue contains
`Number`, `Title`, `State`, optional `Author`, `Comments`, `Updated`, optional
`Labels`, and `URL`. Entries are separated by `----------`. An empty result returns
`No issues found for <owner>/<repo>.`. A non-successful response returns the GitHub
response body, or the HTTP status line when the body is empty.

All three issue tools use the shared rate-limit coordinator and single controlled
retry described above. Their returned strings use the runner's standard tool-result
persistence and truncation path.

### `github_list_releases`

Lists releases via `GET /repos/{owner}/{repo}/releases`. `owner` and `repo` are
required. Optional integer `perPage` selects a page size between 1 and 100. The
result starts with `GitHub Releases for <owner>/<repo> (showing <n>)`. Each release
contains `Tag`, optional `Name`, `Draft`, `Pre-release`, `Published`, and `URL`.
Entries are separated by `----------`; a null published date is rendered as `N/A`.
An empty result returns `No releases found for <owner>/<repo>.`. A non-successful
response returns the GitHub response body, or the HTTP status line when the body is
empty. Both release tools use the shared rate-limit coordinator and single
controlled retry described above. Their returned strings use the runner's standard
tool-result persistence and truncation path.

### `github_list_pull_requests`

Lists pull requests via `GET /repos/{owner}/{repo}/pulls`. `owner` and `repo`
are required. Optional `state`, `head`, `base`, `sort`, and `direction` values
are forwarded as GitHub filters. Optional integer `perPage` selects a page size
between 1 and 100, and optional integer `page` selects a 1-based page. The
result starts with
`GitHub Pull Requests for <owner>/<repo> (showing <n>)`. Each pull request
contains `Number`, `Title`, `State` with an optional `(draft)` marker, optional
`Author`, `Base`, and `Head`, then `Updated`, optional `Merged At`, and `URL`.
Entries are separated by `----------`. An empty result returns
`No pull requests found for <owner>/<repo>.`. A non-successful response returns
the GitHub response body, or the HTTP status line when the body is empty.

### `github_pull_request_read`

Reads a pull request or related data. `method`, `owner`, `repo`, and the integer
`pullNumber` are required. `method` is one of `get`, `get_diff`, `get_status`,
`get_files`, `get_review_comments`, `get_reviews`, or `get_comments`. Optional
integer `perPage` between 1 and 100 and optional 1-based integer `page` are sent
to list endpoints.

- `get` reads `GET /repos/{owner}/{repo}/pulls/{pull_number}` and returns the
  pull request fields and body in the reference Markdown layout.
- `get_diff` reads the same endpoint with
  `Accept: application/vnd.github.v3.diff` and returns a fenced `diff` block.
- `get_status` reads the pull request head SHA and then
  `GET /repos/{owner}/{repo}/commits/{sha}/status`.
- `get_files` reads
  `GET /repos/{owner}/{repo}/pulls/{pull_number}/files` and returns file change
  counts, optional patches, and optional blob/raw URLs.
- `get_review_comments` reads
  `GET /repos/{owner}/{repo}/pulls/{pull_number}/comments`.
- `get_reviews` reads
  `GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews`.
- `get_comments` reads
  `GET /repos/{owner}/{repo}/issues/{pull_number}/comments`.

Review and issue comment bodies are trimmed to 400 characters with `…` when
longer. Empty list results are `No files.`, `No review comments.`, `No reviews.`,
or `No issue comments.` inside the method-specific result header. Every request,
including diff retrieval, uses the shared rate-limit coordinator and the single
controlled retry described above. A non-successful response returns the GitHub
response body, or the HTTP status line when the body is empty. Returned strings
use the runner's standard tool-result persistence and truncation path.

## Catalog vs API schemas

The registry exposes two independent surfaces:

- **`list_catalog()`** — every allowed tool as a
  `{ name, description }` pair, sorted by name. It ignores lazy schema
  visibility, so the discovery catalog is always complete.
- **`list_schemas()`** — the full JSON parameter schemas, filtered by lazy
  visibility. This is what is sent to the provider as the API tool list (native
  mode) or embedded in the prompt (text mode).

The system prompt's **`## Available Tools`** section is built from
`list_catalog()` and lists every allowed tool by a JSON-name label so the
identifier is unambiguous:

```text
## Available Tools

- `{"name":"edit_file"}`: Exact-match string replacement in a file...
- `{"name":"ripgrep"}`: Search files with ripgrep...
- `{"name":"get_tool"}`: Fetch the full parameter schema...
```

This format is identical in native and text mode, and in the live, debug, and UI
prompt surfaces. In text mode the parameter schemas follow in a separate
**`## Tool Schemas`** block (headings use the same `{"name":"<tool>"}` label),
because text mode can't send schemas through the provider API.

## Lazy Registry Mode

By default every LLM turn includes full JSON schemas for all registered tools.
With many MCP servers this can burn 15,000+ tokens per turn. **Lazy mode** keeps
the full catalog advertised but defers the parameter schemas: only the
`get_tool` meta-tool and schemas the model has fetched by exact name are sent.

### Configuration

```toml
[tools]
registry_mode = "lazy"   # default: "full"
```

### How it works

1. `Available Tools` still lists every allowed tool by name (the full catalog),
   plus `get_tool`. Only `get_tool`'s parameter schema is sent initially.
2. `get_tool(name="memory_search")` returns that tool's full schema and makes it
   visible. `get_tool` takes exactly one argument, `name` — an exact tool name
   from `Available Tools`. There is no keyword search, and any other field is
   rejected. An unknown name returns a structured `schema_visible: false`
   response rather than an execution error.
3. `get_tool(name="get_tool")` is a valid lookup that returns the meta-tool's
   own schema.
4. Allowed tools remain executable before their schema is revealed. Once the
   model knows the exact tool name and parameters, it should call the tool
   directly — standard pipeline, hooks fire normally. `get_tool` is not an
   execution permission step and should not be repeated for the same tool.

The runner re-computes schemas each model round, so revealed schemas appear
immediately. On later turns, lazy visibility is restored from structured session
history: prior successful `get_tool` schema reveals (`tool_result` with
`tool_name == "get_tool"`, `success == true`, and
`result.schema_visible == true`) and prior assistant tool calls keep those
schemas visible. The restoration is not inferred from user or assistant prose,
and older sessions that predate `get_tool` simply start from `{get_tool}`.
Every LLM-emitted `get_tool` invocation consumes one unit from the active
agent's `max_tools_threshold`, just like any other tool call. Calling the
revealed target tool consumes another unit. Lazy mode does not increase the
threshold.

`get_tool` is a reserved control-plane name: enabling lazy mode fails cleanly if
a user or MCP tool is already named `get_tool`, and the existing tool is left
untouched.

### When to use

- Many MCP servers connected (50+ tools)
- Long conversations where input token cost matters
- Sub-agent runs that only need a few specific tools

In **full** mode (default), all schemas are sent every turn — no behavioral
change from before this feature.

## Interactive A2UI tool

The built-in `render_a2ui` tool publishes a strict, provider-compatible schema
for A2UI v0.9.1 server messages. It renders one trusted basic-catalog surface
in web chat and waits for a standard event action. In lazy mode, reveal it once
with `get_tool(name="render_a2ui")`, then call it directly.

The tool uses implementation validation in addition to the lightweight JSON
schema validator. This enforces message ordering, one surface, the fixed basic
catalog, component limits, and the presence of an event action before a
caller-visible start event is emitted.

See [Generative UI with A2UI](a2ui.md) for the complete tool contract and
examples.
