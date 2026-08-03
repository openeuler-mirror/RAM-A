# Case-library tool instruction snippet

Use this snippet in the MCP client's system prompt, role prompt, or demo prompt when the
client supports custom instructions.

```text
When the user asks about troubleshooting, incident diagnosis, root-cause analysis,
remediation steps, operational case lookup, or whether a similar historical case exists,
first call the RAM-A MCP tool `memory_case_search`.

Also use this tool when the user asks whether there were similar past cases, previous
incidents, known fixes, or examples for a troubleshooting symptom, even if they do not
explicitly say "case library" or name the tool.

Use the returned case references as the primary evidence for the answer. Mention the
source names returned by the tool. If the tool returns no relevant references or fails,
state that clearly before falling back to general troubleshooting knowledge.
```
