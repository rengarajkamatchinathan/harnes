Summarise the conversation so far so that it can continue from your summary alone. Summarise only what the user and assistant said and did. Leave out instructions, memory and environment; they are sent again. Use exact names, paths and ids.

Use this template, with every heading present (write "None" under a heading with nothing to say):

## Goal
## User preferences & constraints
## Decisions
## Current state
## Open threads
## Key references{if focus}

Pay particular attention to: {focus}{/if}{if request}

The user's newest message below is still being worked on and is not shown again after your summary. Put it under ## Goal as the current task, quoted exactly, and record under ## Current state what has been done for it so far:

<newest_message>
{request}
</newest_message>{/if}
