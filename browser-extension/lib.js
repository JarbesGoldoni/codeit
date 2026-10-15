// The extension's logic that doesn't need the browser (unit-tested with `bun test`).

/** Whether a cookie set for `domain` is sent to `host` (a leading dot means subdomains too). */
export function domainMatches(host, domain) {
  const d = domain.replace(/^\./, "").toLowerCase();
  const h = host.toLowerCase();
  return h === d || h.endsWith(`.${d}`);
}

/** Whether a site asks for the cookie `name` (`JSESSIONID.*` asks for every `JSESSIONID.` one). */
export function wants(site, name) {
  const wanted = site.cookies ?? [];
  return wanted.length === 0 || wanted.some((w) => (w.endsWith("*") ? name.startsWith(w.slice(0, -1)) : w === name));
}

/** The cookies a site asks for (all of them when it names none), as codeit takes them. */
export function pick(site, cookies) {
  return cookies
    .filter((c) => wants(site, c.name))
    .map((c) => ({ name: c.name, value: c.value, domain: c.domain }));
}

/** A string that changes when any of the cookies does (order doesn't matter). */
export function fingerprint(cookies) {
  return cookies
    .map((c) => `${c.domain}\u0000${c.name}\u0000${c.value}`)
    .sort()
    .join("\u0001");
}

/** The sites a changed cookie belongs to. */
export function sitesFor(sites, cookie) {
  return sites.filter((s) => {
    let host;
    try {
      host = new URL(s.url).hostname;
    } catch {
      return false;
    }
    return domainMatches(host, cookie.domain) && wants(s, cookie.name);
  });
}

/** A short name for this browser, for codeit's list. */
export function browserName(ua) {
  if (/Edg\//.test(ua)) return "Edge";
  if (/OPR\//.test(ua)) return "Opera";
  if (/Chrome\//.test(ua)) return "Chrome";
  return "browser";
}
