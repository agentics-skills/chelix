# DuckDuckGo Search

Chelix provides the built-in `duckduckgo_search` tool. It searches the DuckDuckGo HTML endpoint and returns titles, snippets, and URLs as Markdown.

## Tool contract

Description exposed to the model:

> Search the web using DuckDuckGo

Parameters:

| Parameter | Type | Required | Description |
| --- | --- | --- | --- |
| `query` | string | yes | The search query |
| `page` | integer, minimum 1 | no | Page number (starts at 1). Default: `1`. |
| `numResults` | integer, 1–20 | no | Number of results to return (default: 10, max: 20). |

Unknown model-facing parameters are rejected. Runtime metadata keys beginning with `_` are removed before strict parameter deserialization.

## Result format

A successful non-empty result is returned as Markdown in this exact shape:

```markdown
Search results for "<query>":

Page <page>; showing <count> result(s)

1. [<title>](<url>)
   <snippet>
   Source: <display URL>
```

When DuckDuckGo returns no parsed results, the tool returns `No results found.`

The agent runner persists this complete agent-facing string through the standard tool-result store before applying the configured in-context truncation limit.

## Queue, retry, cookies, and timeout

Every registered invocation shares one client, one FIFO request gate, and one in-memory cookie jar. Requests are serialized. Starts are separated by at least five seconds; queue positions after the first three increase the interval by five seconds per position, matching the source tool.

A rate-limited response is retried once while the call retains the serial queue permit, so its retry delay also delays queued calls. A numeric `Retry-After` value controls that delay, with the shared five-second interval as the minimum; otherwise the five-second interval is used. Successful responses use the same retry path when the body is shorter than 1,000 bytes or structural HTML `class`, `id`, or `name` attributes contain `captcha`, `blocked`, or `anomaly-modal`; those words in result text do not trigger block detection. Cookies received by one response are reused by later requests for the lifetime of the shared client.

`tools.duckduckgo.request_timeout_secs` is one deadline for the complete tool call. It includes waiting for the shared queue, request spacing, retry/backoff, the HTTP request, and reading the response body. It is not restarted for each request attempt.

```toml
[tools.duckduckgo]
request_timeout_secs = 300
```

The value must be at least `1`.

The current transport uses one fixed Chrome user agent. HTTP execution is isolated behind the DuckDuckGo crate's transport boundary so the request implementation can be replaced without changing the tool contract, parser, queue, or result formatting.
