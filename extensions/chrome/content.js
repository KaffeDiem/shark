// An "Open in Shark" button in GitHub's PR header, kept through GitHub's in-page navigation.
// Falls back to a floating button when the header has no visible actions container.
const PR_PAGE = /^https:\/\/[^/]+\/[^/]+\/[^/]+\/pull\/\d+/;
const ID = "open-in-shark";

function place() {
  const match = PR_PAGE.exec(location.href);
  const old = document.getElementById(ID);
  if (!match) return old?.remove();
  const anchor = [...document.querySelectorAll('[data-component="PH_Actions"], .gh-header-actions')]
    .find((a) => a.childElementCount > 0);
  if (old && (anchor ? anchor.contains(old) : old.isConnected)) return;
  old?.remove();
  const button = document.createElement("button");
  button.id = ID;
  button.type = "button";
  button.textContent = "Open in Shark";
  Object.assign(button.style, {
    font: "inherit", fontSize: "14px", fontWeight: "500", height: "32px", padding: "0 12px",
    borderRadius: "6px", cursor: "pointer", border: "1px solid var(--borderColor-default, #d0d7de)",
    background: "var(--button-default-bgColor-rest, #f6f8fa)", color: "var(--fgColor-default, #1f2328)",
  });
  button.onclick = () => { location.href = "shark://open?url=" + encodeURIComponent(match[0]); };
  if (anchor) return anchor.prepend(button);
  Object.assign(button.style, { position: "fixed", right: "16px", bottom: "16px", zIndex: "1000" });
  document.body.append(button);
}

let queued = false;
new MutationObserver(() => {
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => { queued = false; place(); });
}).observe(document.documentElement, { childList: true, subtree: true });
place();
