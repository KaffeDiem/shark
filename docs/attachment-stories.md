# Attachment user stories

Stories for connecting Linear and for attaching Linear issues and repositories to an agent. They come from a code audit of the first version and from patterns in comparable products. Stories are ordered from most to least fundamental within each group. `design.md` holds the visual rules these stories must follow.

## Finding a Linear issue

1. **As a** developer starting a task, **I want** to see my assigned and recently updated issues as soon as the picker opens, **so that** I can attach the likely issue without typing.
2. **As a** developer looking for an issue, **I want** results to update as I type, **so that** I can narrow the list without stopping to submit a search.
3. **As a** developer scanning results, **I want** to tell each issue's state, priority, owner and age at a glance, **so that** I can pick the right issue without opening it.
4. **As a** developer who knows the issue, **I want** a pasted URL or typed identifier to find that exact issue, **so that** I don't have to search for it.
5. **As a** developer searching for a term that looks like an identifier, such as `gpt-4`, **I want** a normal search to run as well, **so that** the term isn't treated only as an issue number.
6. **As a** developer on a slow connection, **I want** the current results to stay visible while new ones load, **so that** the list doesn't flicker and I can keep reading.
7. **As a** developer whose search failed, **I want** my query and last results kept with a way to retry, **so that** a failure doesn't cost me my place.
8. **As a** developer who hit Linear's rate limit, **I want** the search to wait and resume on its own, **so that** I don't have to retry by hand or make the limit worse.
9. **As a** developer attaching related work, **I want** to attach several issues without reopening the picker, **so that** gathering context takes one pass.
10. **As a** developer viewing results, **I want** to see which issues are already attached, **so that** I don't try to attach one twice.
11. **As a** keyboard user, **I want** the picker to show its keys, **so that** I can learn it without reading the manual.

## Working with attached issues

12. **As a** developer in an agent, **I want** each attached issue's state and identifier visible in the header, **so that** I know what the agent is working from.
13. **As a** developer checking an issue, **I want** a readable summary with its status, owner and last update, **so that** I don't have to leave Shark.
14. **As a** developer with several issues, **I want** to know what "primary" does before I change it, **so that** I can choose it deliberately.
15. **As a** developer who pressed Refresh, Make primary or Remove, **I want** to see that it worked, **so that** I don't repeat the action.
16. **As a** developer whose issue snapshot is missing, **I want** to see the issue's identifier and a way to fetch it again, **so that** the attachment can be repaired.
17. **As a** developer starting from an issue, **I want** to know the agent will plan first and has no repository yet, **so that** nothing it does surprises me.

## Attaching repositories

18. **As a** developer attaching a repository, **I want** to see each repository with its branch and base before confirming, **so that** I know what will be created.
19. **As a** developer choosing a base, **I want** that choice to be easy to find, **so that** I don't need a hidden shortcut.
20. **As a** developer waiting on an attachment, **I want** to see which step it's on and what it's waiting for, **so that** I know whether I need to act.
21. **As a** developer whose attachment failed, **I want** to read the error and the setup log and retry from one place, **so that** I can fix it without hunting.
22. **As a** developer with several queued repositories, **I want** to cancel one without cancelling the rest, **so that** I keep the work I still want.
23. **As a** developer with a chat on hold, **I want** the chat to say what it's waiting for and what I can do, **so that** I'm not left waiting on something that needs me.
24. **As a** developer with many attachments, **I want** to reach everything that doesn't fit in the header, **so that** a failure is never hidden.

## Connecting Linear

25. **As a** new user, **I want** to see one clear way to connect and what Shark can access, **so that** I can trust the connection.
26. **As a** user signing in through the browser, **I want** to see that Shark is waiting and be told when it finishes, **so that** I know the result even if I've left Settings.
27. **As a** user who mistyped an API key, **I want** to correct it in place, **so that** I don't have to paste it again.
28. **As a** user creating an API key, **I want** a direct link to the right Linear page, **so that** I don't have to look for it.
29. **As a** user whose access was revoked, **I want** a clear prompt to reconnect wherever I hit the problem, **so that** a healthy-looking connection doesn't mislead me.
30. **As a** user about to disconnect or switch workspace, **I want** to know what happens to my attached issues, **so that** I don't lose access by accident.
31. **As a** user who cancelled sign-in, **I want** that treated as my choice rather than an error, **so that** the app doesn't suggest something went wrong.

## Attachment dialogs

32. **As a** developer attaching context, **I want** one place to start, **so that** I don't have to choose between controls before I know what I need.
33. **As a** developer who doesn't use Linear, **I want** no Linear controls in my way, **so that** the app only shows what applies to me.
34. **As a** developer who wants Linear, **I want** to find it in Settings, **so that** I can turn it on when I choose to.
35. **As a** developer who disconnected Linear, **I want** my attached issues to stay readable with a way to reconnect, **so that** I don't lose my context.
36. **As a** developer attaching a repository, **I want** the common case to take one key, **so that** a routine step stays quick.
37. **As a** developer moving between the two dialogs, **I want** the same keys and layout in each, **so that** I don't have to relearn them.
38. **As a** developer attaching several repositories, **I want** to see how many I'm attaching before I confirm, **so that** I attach what I intended.
39. **As a** developer with a usual branch and base, **I want** those details kept out of the way until I need them, **so that** the dialog stays short.
40. **As a** developer whose branch name is taken, **I want** the problem stated plainly with a working alternative, **so that** I can continue without guessing.
41. **As a** developer attaching a later repository, **I want** to see that it follows the agent's branch, **so that** I don't change it by accident.
42. **As a** developer picking a repository, **I want** to see which are already attached and which I used most recently, **so that** I can find the right one quickly.
43. **As a** new user with no repositories, **I want** to go straight to choosing a folder, **so that** I don't land on an empty list.
44. **As a** developer adding a local repository, **I want** to come back to the dialog as I left it, even if I cancel, **so that** I don't lose my choices.
45. **As a** developer reviewing what's attached, **I want** repositories and issues listed together, **so that** I can see the whole picture in one place.
