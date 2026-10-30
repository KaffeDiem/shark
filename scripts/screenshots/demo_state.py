import json, os, sys
H = sys.argv[1]
S = H + '/Library/Application Support/shark'
for d in ('transcripts', 'plans'):
    os.makedirs(f'{S}/{d}', exist_ok=True)
dev = H + '/Developer'

def ws(name, repos, chats, base='origin/main'):
    return {'name': name, 'branch': name,
            'repos': [{'name': r, 'src': f'{dev}/{r}', 'base': base} for r in repos],
            'chats': chats, 'closed': [], 'active_chat': 0, 'viewed': {}}

state = {
    'repos': [{'name': r, 'path': f'{dev}/{r}'} for r in ('ios', 'backend', 'web')],
    'workspaces': [
        ws('eng-123-login', ['ios', 'backend'],
           [{'id': 'eng-123-login', 'title': 'Rate-limit login and show errors in the app', 'session_id': 'demo-1', 'model': 'fable', 'effort': 'high'}]),
        ws('eng-118-push-notifications', ['ios'],
           [{'id': 'eng-118-push-notifications', 'title': 'Add push notifications for new messages', 'session_id': 'demo-2', 'model': 'opus', 'effort': 'high', 'plan': True}]),
        ws('eng-130-rate-limits', ['backend', 'web'], [{'id': 'eng-130-rate-limits', 'model': 'fable'}]),
        ws('eng-127-onboarding-copy', ['web'], [{'id': 'eng-127-onboarding-copy', 'model': 'sonnet'}]),
    ],
    'last_repos': ['ios', 'backend'],
    'last_open': ['eng-123-login', 'chat'],
    'appearance': {'mode': 'dark', 'light': 'Linear', 'dark': 'Linear'},
    'home_checklist_dismissed': True, 'palette_used': True, 'opened_pr': True, 'approved_plan': True,
    'last_seen_version': '9.9.9', 'last_model': 'fable', 'last_effort': 'high',
}
if sys.argv[2:] == ['stack']:  # run.sh's stack shot: two workspaces of setup.sh's ios stack
    state['workspaces'] += [
        ws('eng-141-sync-ui', ['ios'], [{'id': 'eng-141-sync-ui', 'model': 'fable'}], 'origin/eng-140-sync-logic'),
        ws('eng-142-sync-wiring', ['ios'], [{'id': 'eng-142-sync-wiring', 'model': 'fable'}], 'origin/eng-141-sync-ui'),
    ]
    state['last_open'] = ['eng-141-sync-ui', 'chat']
json.dump(state, open(f'{S}/state.json', 'w'), indent=1)

def user(text, sid):
    return {'type': 'user', 'message': {'role': 'user', 'content': text}, 'session_id': sid}
def asst(content, sid):
    return {'type': 'assistant', 'message': {'model': 'claude-fable-5-1', 'type': 'message', 'role': 'assistant', 'content': content}, 'session_id': sid}
def tool(id, name, inp):
    return {'type': 'tool_use', 'id': id, 'name': name, 'input': inp}
def res(id, out, sid):
    return {'type': 'user', 'message': {'role': 'user', 'content': [{'tool_use_id': id, 'type': 'tool_result', 'content': out, 'is_error': False}]}, 'session_id': sid}
def result(sid):
    return {'type': 'result', 'subtype': 'success', 'is_error': False, 'duration_ms': 48000, 'num_turns': 6, 'result': '', 'session_id': sid, 'total_cost_usd': 0.41}

ws_dir = H + '/shark/workspaces/eng-123-login'
sid = 'demo-1'
t = [
    {'type': 'system', 'subtype': 'init', 'cwd': ws_dir, 'session_id': sid, 'model': 'claude-fable-5-1'},
    user('Rate-limit login attempts in the backend and show the error in the iOS login screen. Save the session token in the Keychain.', sid),
    asst([{'type': 'text', 'text': "I'll add a limiter check to the login handler, then surface errors and a loading state in the iOS view model."}], sid),
    asst([tool('t1', 'Read', {'file_path': ws_dir + '/backend/internal/auth/login.go'})], sid),
    res('t1', 'package auth\n…', sid),
    asst([tool('t2', 'Edit', {'file_path': ws_dir + '/backend/internal/auth/login.go', 'old_string': '\tuser, err :=', 'new_string': '\tif !s.limiter.Allow(clientIP(r)) {'})], sid),
    res('t2', 'The file has been updated.', sid),
    asst([tool('t3', 'Edit', {'file_path': ws_dir + '/ios/App/Login/LoginViewModel.swift', 'old_string': '_ = try? await', 'new_string': 'isLoading = true'})], sid),
    res('t3', 'The file has been updated.', sid),
    asst([tool('t4', 'Write', {'file_path': ws_dir + '/ios/App/Networking/Keychain.swift', 'content': 'import Security\n…'})], sid),
    res('t4', 'File created.', sid),
    asst([tool('t5', 'Bash', {'command': 'cd backend && go test ./internal/auth/...', 'description': 'Run the auth tests'})], sid),
    res('t5', 'ok  \texample.com/backend/internal/auth\t0.412s', sid),
    asst([{'type': 'text', 'text': "Done. Changes across both repos:\n\n- **backend** `internal/auth/login.go`: login returns `429 Too Many Requests` once the limiter refuses the client's IP, and a failed session creation is a `500` instead of a panic.\n- **ios** `LoginViewModel.swift`: `isLoading` and `error` drive the view; the token goes to the Keychain on success.\n- **ios** `LoginView.swift`: shows the error under the fields and disables **Sign in** while a request runs.\n- **ios** `Keychain.swift`: a small wrapper around `SecItemAdd`.\n\nThe auth tests pass. Next step is committing and opening linked PRs."}], sid),
    result(sid),
]
with open(f'{S}/transcripts/eng-123-login.jsonl', 'w') as f:
    for l in t: f.write(json.dumps(l) + '\n')

v1 = """# Push notifications for new messages

1. Register for remote notifications in `AppDelegate` after the user signs in.
2. Send the device token to the backend with `POST /devices`.
3. Show a banner for a new message while the app is in the foreground.

Tests: a unit test for the token upload.
"""
v2 = """# Push notifications for new messages

1. Ask for notification permission after the first message is sent, not at sign-in.
2. Register for remote notifications in `AppDelegate` and send the device token to the backend with `POST /devices`, retrying once on failure.
3. Show a banner for a new message while the app is in the foreground, unless that conversation is open.
4. Open the conversation when a notification is tapped.

```swift
func application(_ app: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken token: Data) {
    Task { try await API.shared.registerDevice(token: token.hex, retries: 1) }
}
```

Tests: unit tests for the token upload and the tap routing.
"""
json.dump({'versions': [{'id': 'p1', 'text': v1}, {'id': 'p2', 'text': v2}]}, open(f'{S}/plans/eng-118-push-notifications.json', 'w'))
ws2 = H + '/shark/workspaces/eng-118-push-notifications'
sid = 'demo-2'
t = [
    {'type': 'system', 'subtype': 'init', 'cwd': ws2, 'session_id': sid, 'model': 'claude-opus-5-5'},
    user('Add push notifications for new messages.', sid),
    asst([tool('p1', 'ExitPlanMode', {'plan': v1})], sid),
    res('p1', 'The user is reviewing the plan.', sid),
    result(sid),
    user("Don't ask for permission at sign-in, wait until they send their first message. Also handle tapping the notification.", sid),
    asst([tool('p2', 'ExitPlanMode', {'plan': v2})], sid),
    res('p2', 'The user is reviewing the plan.', sid),
    result(sid),
]
with open(f'{S}/transcripts/eng-118-push-notifications.jsonl', 'w') as f:
    for l in t: f.write(json.dumps(l) + '\n')

# Review threads on eng-123-login's diff: one the agent addressed, one still pending.
os.makedirs(f'{S}/comments', exist_ok=True)
login = 'backend/internal/auth/login.go'
comments = [
    {'id': 1, 'path': login, 'start': [False, 26], 'end': [False, 26], 'rev': 'working tree',
     'text': "Don't drop the error here; a failed session should be a 500.",
     'snapshot': '\tsession, err := s.sessions.New(r.Context(), user)',
     'status': {'Addressed': {'turn': 0, 'note': 'Returns 500 "could not create session" now.'}}, 'chat': 'eng-123-login'},
    {'id': 2, 'path': login, 'start': [False, 17], 'end': [False, 20], 'rev': 'working tree',
     'text': 'Send Retry-After so the app can say when to try again.',
     'snapshot': '\tif !s.limiter.Allow(clientIP(r)) {\n\t\thttp.Error(w, "too many attempts", http.StatusTooManyRequests)\n\t\treturn\n\t}',
     'suggestion': '\tif !s.limiter.Allow(clientIP(r)) {\n\t\tw.Header().Set("Retry-After", "60")\n\t\thttp.Error(w, "too many attempts", http.StatusTooManyRequests)\n\t\treturn\n\t}'},
]
json.dump({'last': 2, 'comments': comments}, open(f'{S}/comments/eng-123-login.json', 'w'))

# 20 weeks of agent logs for the usage calendar: weekdays busy, weekends light.
import math, time
now = time.time()
def day(i):  # tokens i days ago
    wd = time.localtime(now - i * 86400).tm_wday
    return 0 if wd == 6 or (i * 7) % 11 == 0 else int((40 if wd < 5 else 8) * (1 + math.sin(i)) * 1e5)
stamp = lambda i: time.strftime('%Y-%m-%dT12:00:00Z', time.gmtime(now - i * 86400))
os.makedirs(H + '/.claude/projects/demo', exist_ok=True)
with open(H + '/.claude/projects/demo/demo.jsonl', 'w') as f:
    for i in range(140):
        if day(i):
            f.write(json.dumps({'type': 'assistant', 'timestamp': stamp(i), 'requestId': f'r{i}', 'message': {'id': f'm{i}', 'usage': {'input_tokens': day(i), 'output_tokens': day(i) // 20}}}) + '\n')
os.makedirs(H + '/.codex/sessions/demo', exist_ok=True)
with open(H + '/.codex/sessions/demo/demo.jsonl', 'w') as f:
    for i in range(0, 140, 3):
        n = day(i) // 3
        if n:
            u = {'input_tokens': n, 'cached_input_tokens': 0, 'output_tokens': n // 20}
            f.write(json.dumps({'timestamp': stamp(i), 'type': 'event_msg', 'payload': {'type': 'token_count', 'info': {'total_token_usage': u, 'last_token_usage': u}}}) + '\n')
