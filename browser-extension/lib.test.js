import { expect, test } from "bun:test";
import { browserName, domainMatches, fingerprint, pick, sitesFor } from "./lib.js";

test("matches cookie domains like the browser does", () => {
  expect(domainMatches("app.example.com", ".example.com")).toBe(true);
  expect(domainMatches("app.example.com", "example.com")).toBe(true);
  expect(domainMatches("app.example.com", "app.example.com")).toBe(true);
  expect(domainMatches("evilexample.com", "example.com")).toBe(false);
  expect(domainMatches("example.com", "app.example.com")).toBe(false);
});

test("keeps only the cookies a site asks for", () => {
  const all = [
    { name: "session", value: "1", domain: ".x.com", httpOnly: true },
    { name: "theme", value: "dark", domain: ".x.com" },
  ];
  expect(pick({ cookies: ["session"] }, all)).toEqual([{ name: "session", value: "1", domain: ".x.com" }]);
  expect(pick({ cookies: [] }, all).length).toBe(2);
  expect(pick({ cookies: ["JSESSIONID.*"] }, [{ name: "JSESSIONID.ab12", value: "v", domain: "j" }, ...all])).toEqual([
    { name: "JSESSIONID.ab12", value: "v", domain: "j" },
  ]);
});

test("fingerprints ignore order and see value changes", () => {
  const a = { name: "a", value: "1", domain: "x" };
  const b = { name: "b", value: "2", domain: "x" };
  expect(fingerprint([a, b])).toBe(fingerprint([b, a]));
  expect(fingerprint([a, { ...b, value: "3" }])).not.toBe(fingerprint([a, b]));
});

test("routes a changed cookie to its sites", () => {
  const sites = [
    { key: "p:jira", url: "https://jira.example.com/", cookies: ["tenant.session.token"] },
    { key: "p:other", url: "https://other.org/", cookies: [] },
  ];
  expect(sitesFor(sites, { name: "tenant.session.token", domain: ".example.com" }).map((s) => s.key)).toEqual(["p:jira"]);
  expect(sitesFor(sites, { name: "theme", domain: ".example.com" })).toEqual([]);
  expect(sitesFor(sites, { name: "x", domain: "other.org" }).map((s) => s.key)).toEqual(["p:other"]);
});

test("names the browser", () => {
  expect(browserName("Mozilla/5.0 ... Chrome/130.0 Safari/537.36 Edg/130.0")).toBe("Edge");
  expect(browserName("Mozilla/5.0 ... Chrome/130.0 Safari/537.36")).toBe("Chrome");
});
