import { expect, test } from "bun:test";
import { parseDiff } from "./diff";
import { fingerprint, quoteOf, reviewText, viewState, type Comment } from "./review";

const patch = (line: string) => `diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
 fn a() {
-    1
+    ${line}
`;

test("a viewed file shows again once it changes", () => {
  const [f] = parseDiff(patch("2"));
  const viewed = { "a.rs": fingerprint(f) };
  expect(viewState(viewed, f)).toBe("viewed");
  expect(viewState(viewed, parseDiff(patch("3"))[0])).toBe("changed");
  expect(viewState({}, f)).toBe("new");
});

test("the review quotes the lines and puts the summary last", () => {
  const [f] = parseDiff(patch("2"));
  expect(quoteOf(f, 1, 2)).toBe("fn a() {\n    2");
  const c: Comment[] = [
    { id: "2", path: "b.rs", from: null, to: null, rev: null, quote: "", text: "Split this file." },
    { id: "1", path: "a.rs", from: 2, to: 2, rev: "abcdef123", quote: "    2", text: "Why 2? " },
  ];
  expect(reviewText(c, "Looks good otherwise.", "the branch `x`")).toBe(
    [
      "My code review of the branch `x`. Address each comment; ask if one is unclear.",
      "",
      "a.rs:2 (at abcdef1):",
      ">     2",
      "Why 2?",
      "",
      "b.rs (the whole file):",
      "Split this file.",
      "",
      "Overall: Looks good otherwise.",
    ].join("\n"),
  );
});
