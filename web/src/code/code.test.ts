import { expect, test } from "bun:test";
import { parseDiff } from "./diff";
import { buildTree, parents } from "./tree";
import { fileIcon } from "./icons";

const PATCH = `diff --git a/src/lib.rs b/src/lib.rs
index 1..2 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@ fn add
 pub fn add(a: i32) -> i32 {
-    a + 1
+    a + 2
+    // two
 }
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
diff --git a/img.png b/img.png
new file mode 100644
Binary files /dev/null and b/img.png differ
`;

test("reads a patch", () => {
  const files = parseDiff(PATCH);
  expect(files.map((f) => [f.path, f.status, f.added, f.removed, f.binary])).toEqual([
    ["src/lib.rs", "modified", 2, 1, false],
    ["new.txt", "renamed", 0, 0, false],
    ["img.png", "added", 0, 0, true],
  ]);
  const h = files[0].hunks[0];
  expect(h.header).toBe("fn add");
  expect(h.lines.map((l) => [l.kind, l.old, l.new])).toEqual([
    ["ctx", 1, 1],
    ["del", 2, null],
    ["add", null, 2],
    ["add", null, 3],
    ["ctx", 3, 4],
  ]);
  expect(files[1].oldPath).toBe("old.txt");
});

test("builds the tree, joining single folders", () => {
  const t = buildTree(["src/main/java/A.java", "src/main/java/B.java", "README.md", "web/src/a.ts", "web/b.ts"]);
  expect(t.map((n) => n.name)).toEqual(["src/main/java", "web", "README.md"]);
  expect(t[0].path).toBe("src/main/java");
  expect(t[1].children.map((n) => n.name)).toEqual(["src", "b.ts"]);
  expect(parents("a/b/c.ts")).toEqual(["a", "a/b"]);
});

test("picks icons like VS Code", () => {
  const theme = {
    iconDefinitions: { f: { iconPath: "f.svg" }, ts: { iconPath: "ts.svg" }, test: { iconPath: "test.svg" }, pkg: { iconPath: "pkg.svg" }, rs: { iconPath: "rs.svg" } },
    file: "f",
    fileExtensions: { ts: "ts", "test.ts": "test" },
    fileNames: { "package.json": "pkg" },
    languageIds: { rust: "rs" },
  };
  expect(fileIcon(theme, "src/a.ts")).toBe("/skin/ts.svg");
  expect(fileIcon(theme, "a.test.ts")).toBe("/skin/test.svg");
  expect(fileIcon(theme, "web/Package.json")).toBe("/skin/pkg.svg");
  expect(fileIcon(theme, "main.rs")).toBe("/skin/rs.svg");
  expect(fileIcon(theme, "x.unknown")).toBe("/skin/f.svg");
});
