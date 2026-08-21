# Tool Call Bubble

## Tool Call Sections

- `RESULT`: Backend-provided final tool result, or tool error displayed in a separate `RESULT` section.
- `OUTPUT`: Backend-provided `execution_progress`, live terminal or final tool text output displayed in a separate `OUTPUT` section.
- `INTERFACE`: A2UI interactive representation and state displayed in a separate `INTERFACE` section.

## Business Requirements

1. The `RESULT` heading MUST remain hidden until the result section contains content; each `OUTPUT` heading MUST remain hidden until its output block contains content.
2. The `Parameters`, `Raw result payload`, and `Context budget` disclosures MUST be collapsed by default.
