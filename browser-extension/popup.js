const $ = (id) => document.getElementById(id);

function el(tag, text, cls) {
  const e = document.createElement(tag);
  if (text) e.textContent = text;
  if (cls) e.className = cls;
  return e;
}

async function render() {
  const { status } = await chrome.storage.session.get("status");
  const { port } = await chrome.storage.local.get("port");
  $("port").value = port || status?.port || 7317;
  const connected = !!status?.connected;
  $("state").textContent = connected ? "● connected" : "○ not connected";
  $("state").className = `state ${connected ? "ok" : "bad"}`;
  $("help").textContent = connected
    ? status.sites.length
      ? "Sending these sites' cookies to codeit as they change."
      : "Connected. No plugin asks for browser cookies."
    : `Start codeit with \`codeit web\` (port ${$("port").value}); this connects by itself within a minute.`;
  const list = $("sites");
  list.replaceChildren();
  for (const s of status?.sites ?? []) {
    const li = el("li");
    const r = s.result;
    li.append(el("b", s.title), document.createTextNode(" "));
    li.append(el("span", r ? (r.ok ? "✓ works" : "✗ needs login") : "…", r ? (r.ok ? "ok" : "bad") : "muted"));
    li.append(el("small", r?.detail ?? s.url));
    list.append(li);
  }
}

$("save").onclick = async () => {
  // The extension reconnects when the port changes; this also retries on the same one.
  await chrome.storage.local.set({ port: Number($("port").value) || 7317 });
  await chrome.runtime.sendMessage({ type: "reconnect" });
  setTimeout(render, 500);
};
$("send").onclick = async () => {
  await chrome.runtime.sendMessage({ type: "send-now" });
  setTimeout(render, 800);
};
chrome.storage.session.onChanged.addListener(render);
void render();
