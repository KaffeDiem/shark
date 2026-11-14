// Toolbar button: hand the current pull request page to Shark.
// Shark opens the task that already has this PR, or starts a new one.
const PR_PAGE = /^https:\/\/[^/]+\/[^/]+\/[^/]+\/pull\/\d+/;

chrome.action.onClicked.addListener(async (tab) => {
  const match = tab.url && PR_PAGE.exec(tab.url);
  await chrome.action.setBadgeText({ tabId: tab.id, text: match ? "" : "?" });
  if (!match) return;
  const link = "shark://open?url=" + encodeURIComponent(match[0]);
  // Navigating from the page keeps Chrome's "Open Shark?" prompt with its "Always allow" option.
  await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: (url) => { window.location.href = url; },
    args: [link],
  });
});
