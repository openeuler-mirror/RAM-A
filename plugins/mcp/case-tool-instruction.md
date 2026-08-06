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

Never upload, update, or delete a case while diagnosis is still in progress. After the
diagnosis is complete, first answer the user's troubleshooting question. If the result
is worth preserving as a new case or replacing an existing case, call
`memory_case_prepare_upload` or `memory_case_prepare_update` with the diagnosis summary
and final case content. If an existing case is obsolete, unsafe, or explicitly requested
for removal, call `memory_case_prepare_delete` with the document ID and deletion reason.
These preparation tools do not mutate the case library.

Show the returned operation, library, document ID, and name to the user. For upload or
update, also show the diagnosis summary, content preview, and content SHA-256; for delete,
show the deletion reason. Ask whether they explicitly confirm that exact operation, then
end the turn without calling `memory_case_upload`, `memory_case_update`, or
`memory_case_delete`.

Only after a later user message clearly confirms that exact proposal may you call the
matching final tool with the returned `confirmation_token` and `user_confirmed=true`.
Silence, an ambiguous reply, the original troubleshooting request, or a new unrelated
request is not confirmation. If the user declines, do nothing. If the token expires or
the proposal changes, prepare it again and ask for confirmation again. Never fabricate a
confirmation token or claim that the user confirmed when they did not.
```
