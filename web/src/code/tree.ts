// The file tree from a flat list of paths, with chains of single folders joined ("src/main/java").

export interface TreeNode {
  name: string;
  /** Path from the project root. */
  path: string;
  dir: boolean;
  children: TreeNode[];
}

export function buildTree(files: string[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", dir: true, children: [] };
  const dirs = new Map<string, TreeNode>([["", root]]);
  for (const file of files) {
    const parts = file.split("/");
    let parent = root;
    for (let i = 0; i < parts.length; i++) {
      const path = parts.slice(0, i + 1).join("/");
      const last = i === parts.length - 1;
      if (last) {
        parent.children.push({ name: parts[i], path, dir: false, children: [] });
        break;
      }
      let d = dirs.get(path);
      if (!d) {
        d = { name: parts[i], path, dir: true, children: [] };
        dirs.set(path, d);
        parent.children.push(d);
      }
      parent = d;
    }
  }
  const finish = (n: TreeNode): TreeNode => {
    let node = n;
    // Join a folder whose only child is a folder.
    while (node.dir && node.path && node.children.length === 1 && node.children[0].dir) {
      const c = node.children[0];
      node = { ...c, name: `${node.name}/${c.name}` };
    }
    node.children = node.children.map(finish).sort((a, b) => {
      if (a.dir !== b.dir) return a.dir ? -1 : 1;
      return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
    });
    return node;
  };
  return finish(root).children;
}

/** The folders that hold `path`, so they can be opened to show it. */
export function parents(path: string): string[] {
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join("/"));
}
