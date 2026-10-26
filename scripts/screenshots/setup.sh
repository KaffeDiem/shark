#!/bin/sh
# A demo HOME for the README screenshots: three repos, four workspaces, a finished chat and
# a plan with two versions. Used by run.sh.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
D=${SHARK_DEMO:-/tmp/shark-demo}; H=$D/home
rm -rf "$D"; mkdir -p $H/Developer $D/origins
export HOME=$H GIT_CONFIG_GLOBAL=/dev/null
g() { git -c user.name=demo -c user.email=demo@example.com "$@"; }
mk() { # name, then file:content pairs come from a function
  o=$D/origins/$1.git; git init -q --bare -b main $o
  d=$H/Developer/$1; git init -q -b main $d; cd $d
  $2
  g add -A; g commit -qm init; git remote add origin $o; g push -q origin main; git remote set-head origin main
}
backend() { mkdir -p internal/auth internal/ratelimit; cat > internal/auth/login.go <<'EOF'
package auth

import "net/http"

type loginRequest struct {
	Email    string `json:"email"`
	Password string `json:"password"`
}

// Login checks the credentials and starts a session.
func (s *Service) Login(w http.ResponseWriter, r *http.Request) {
	var req loginRequest
	if err := readJSON(r, &req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	user, err := s.users.ByEmail(r.Context(), req.Email)
	if err != nil || !user.CheckPassword(req.Password) {
		http.Error(w, ErrInvalidCredentials.Error(), http.StatusUnauthorized)
		return
	}
	writeJSON(w, s.sessions.New(user))
}
EOF
printf 'module example.com/backend\n\ngo 1.23\n' > go.mod; echo '# backend' > README.md; }
ios() { mkdir -p App/Login App/Networking; cat > App/Login/LoginView.swift <<'EOF'
import SwiftUI

struct LoginView: View {
    @StateObject var model = LoginViewModel()

    var body: some View {
        Form {
            TextField("Email", text: $model.email)
                .textContentType(.emailAddress)
            SecureField("Password", text: $model.password)
            Button("Sign in") { Task { await model.signIn() } }
                .disabled(!model.canSubmit)
        }
        .padding()
    }
}
EOF
cat > App/Login/LoginViewModel.swift <<'EOF'
import Foundation

@MainActor
final class LoginViewModel: ObservableObject {
    @Published var email = ""
    @Published var password = ""

    var canSubmit: Bool { !email.isEmpty && !password.isEmpty }

    func signIn() async {
        _ = try? await API.shared.login(email: email, password: password)
    }
}
EOF
echo '# ios' > README.md; }
web() { mkdir -p src/onboarding public; cp "$here/../../assets/icon.png" public/logo.png; printf 'export const welcome = "Welcome to Acme";\n' > src/onboarding/copy.ts; echo '# web' > README.md; }
mk backend backend; mk ios ios; mk web web

W=$H/shark/workspaces
wt() { # ws repo
  git -C $H/Developer/$2 worktree add -q --no-track -b $1 $W/$1/$2 origin/HEAD; }
wt eng-123-login ios; wt eng-123-login backend
wt eng-118-push-notifications ios
wt eng-130-rate-limits backend; wt eng-130-rate-limits web
wt eng-127-onboarding-copy web

# A stack in ios, pushed: logic ← ui ← wiring. ui and wiring are workspaces (demo_state.py stack).
st() { # branch file commit-message, content on stdin
  g checkout -qb $1; mkdir -p "$(dirname $2)"; cat > $2; g add -A; g commit -qm "$3"; g push -qu origin $1; }
cd $H/Developer/ios
st eng-140-sync-logic App/Sync/SyncEngine.swift 'Add SyncEngine' <<'EOF'
import Foundation

actor SyncEngine {
    private(set) var lastSync: Date?

    func sync() async throws {
        let changes = try await API.shared.changes(since: lastSync)
        try await Store.shared.apply(changes)
        lastSync = .now
    }
}
EOF
st eng-141-sync-ui App/Settings/SyncSettingsView.swift 'Add sync settings screen' <<'EOF'
import SwiftUI

struct SyncSettingsView: View {
    @AppStorage("syncEnabled") var enabled = true
    let lastSync: Date?

    var body: some View {
        Form {
            Toggle("Sync across devices", isOn: $enabled)
            if let lastSync { LabeledContent("Last synced", value: lastSync, format: .relative(presentation: .named)) }
        }
        .navigationTitle("Sync")
    }
}
EOF
st eng-142-sync-wiring App/AppModel.swift 'Run a sync on launch and from settings' <<'EOF'
import Foundation

@MainActor
final class AppModel: ObservableObject {
    let sync = SyncEngine()

    func didLaunch() { Task { try? await sync.sync() } }
}
EOF
g checkout -q main
git worktree add -q $W/eng-141-sync-ui/ios eng-141-sync-ui
git worktree add -q $W/eng-142-sync-wiring/ios eng-142-sync-wiring

# The feature's changes, uncommitted.
cd $W/eng-123-login/backend
python3 - <<'EOF'
p='internal/auth/login.go'; s=open(p).read()
s=s.replace('''	user, err :=''','''	if !s.limiter.Allow(clientIP(r)) {
		http.Error(w, "too many attempts", http.StatusTooManyRequests)
		return
	}
	user, err :=''')
s=s.replace('''	writeJSON(w, s.sessions.New(user))''','''	session, err := s.sessions.New(r.Context(), user)
	if err != nil {
		http.Error(w, "could not create session", http.StatusInternalServerError)
		return
	}
	writeJSON(w, session)''')
open(p,'w').write(s)
EOF
cd $W/eng-123-login/ios
python3 - <<'EOF'
p='App/Login/LoginView.swift'; s=open(p).read()
s=s.replace('''            Button("Sign in")''','''            if let error = model.error {
                Text(error).foregroundStyle(.red).font(.footnote)
            }
            Button("Sign in")''')
s=s.replace('.disabled(!model.canSubmit)','.disabled(!model.canSubmit || model.isLoading)')
open(p,'w').write(s)
p='App/Login/LoginViewModel.swift'; s=open(p).read()
s=s.replace('''    @Published var password = ""
''','''    @Published var password = ""
    @Published var error: String?
    @Published var isLoading = false
''')
s=s.replace('''        _ = try? await API.shared.login(email: email, password: password)''','''        isLoading = true
        defer { isLoading = false }
        do {
            let session = try await API.shared.login(email: email, password: password)
            try Keychain.save(session.token, for: .sessionToken)
            error = nil
        } catch {
            self.error = "Wrong email or password."
        }''')
open(p,'w').write(s)
EOF
mkdir -p App/Networking; cat > App/Networking/Keychain.swift <<'EOF'
import Security
import Foundation

enum Keychain {
    enum Key: String { case sessionToken }

    static func save(_ value: String, for key: Key) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: key.rawValue,
        ]
        SecItemDelete(query as CFDictionary)
        var item = query
        item[kSecValueData as String] = Data(value.utf8)
        let status = SecItemAdd(item as CFDictionary, nil)
        guard status == errSecSuccess else { throw KeychainError(status: status) }
    }
}

struct KeychainError: Error { let status: OSStatus }
EOF

cd $W/eng-130-rate-limits/backend; mkdir -p internal/ratelimit; printf 'package ratelimit\n\n// Limiter allows n attempts per window.\ntype Limiter struct{ n int }\n' > internal/ratelimit/limiter.go
cd $W/eng-127-onboarding-copy/web; cp "$here/../../assets/icon-dev.png" public/logo.png; printf 'export const welcome = "Welcome to Acme. Set up your team to get started.";\n' > src/onboarding/copy.ts

python3 "$here/demo_state.py" "$H"
