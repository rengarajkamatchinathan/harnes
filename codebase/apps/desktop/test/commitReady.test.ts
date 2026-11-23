import { describe, expect, it } from "vitest";
import { changesFiles, classifyCommand, commitMessage, commitType, diffStats, runSucceeded, turnChecks, type TurnCall } from "../src/ui/code/commitReady";

const sh = (command: string, outcome: TurnCall["outcome"] = "ok", result = ""): TurnCall => ({ name: "shell", input: { command }, outcome, result });

describe("classifyCommand", () => {
  it.each([
    "npm test",
    "npm run test -- --watch=false",
    "pnpm test",
    "yarn test",
    "bun run test",
    "bun test",
    "npx vitest run",
    "vitest",
    "jest --ci",
    "pytest -q tests/",
    "python -m pytest",
    "cargo test -p genie-core",
    "go test ./...",
    "dotnet test",
    "mvn -q test",
    "./gradlew test",
    "cd apps/desktop && bun run test",
  ])("%s is a test run", (cmd) => expect(classifyCommand(cmd)).toBe("test"));
  it.each(["npm run lint", "eslint src", "tsc --noEmit -p .", "bun run typecheck", "cargo clippy --all", "ruff check .", "flake8", "mypy app"])("%s is lint", (cmd) =>
    expect(classifyCommand(cmd)).toBe("lint"),
  );
  it.each(["ls -la", "git status", "cat test.txt", "echo latest", "npm install", "grep -r attest ."])("%s is neither", (cmd) => expect(classifyCommand(cmd)).toBeNull());
});

describe("changesFiles", () => {
  it("edit tools change files when they succeed", () => {
    expect(changesFiles({ name: "edit", input: { path: "a.ts" }, outcome: "ok" })).toBe(true);
    expect(changesFiles({ name: "write", input: {}, outcome: null })).toBe(true);
    expect(changesFiles({ name: "apply_patch", input: {}, outcome: "declined" })).toBe(false);
    expect(changesFiles({ name: "read", input: {}, outcome: "ok" })).toBe(false);
  });
  it("shell commands that write files", () => {
    for (const c of ["sed -i 's/a/b/' x.ts", "echo hi > notes.txt", "rm old.ts", "mv a b", "npx prettier --write src", "git apply p.diff"]) expect(changesFiles(sh(c))).toBe(true);
    for (const c of ["ls", "git status", "bun run test 2>&1", "cat a | grep b", "echo hi"]) expect(changesFiles(sh(c))).toBe(false);
  });
});

describe("runSucceeded / turnChecks", () => {
  it("needs outcome ok and no failure markers", () => {
    expect(runSucceeded({ outcome: "ok", result: "Tests  125 passed (125)" })).toBe(true);
    expect(runSucceeded({ outcome: "ok", result: "Tests  2 failed | 123 passed" })).toBe(false);
    expect(runSucceeded({ outcome: "ok", result: "test result: FAILED. 1 passed; 1 failed" })).toBe(false);
    expect(runSucceeded({ outcome: "ok", result: "src/a.ts(3,1): error TS2304: Cannot find name" })).toBe(false);
    expect(runSucceeded({ outcome: "ok", result: "0 failed, 12 passed" })).toBe(true);
    expect(runSucceeded({ outcome: "error", result: "" })).toBe(false);
  });
  it("summarises a turn; the last run of each kind wins", () => {
    const c = turnChecks([
      { name: "edit", input: { path: "a.ts" }, outcome: "ok", result: "" },
      sh("bun run test", "ok", "1 failed"),
      sh("bun run test", "ok", "all 3 passed"),
      sh("eslint .", "error", "boom"),
    ]);
    expect(c).toMatchObject({ changedFiles: true, tests: "ok", lint: "failed", testCommand: "bun run test", lintCommand: "eslint ." });
  });
  it("declined commands count as not run", () => {
    expect(turnChecks([sh("npm test", "declined")])).toMatchObject({ changedFiles: false, tests: "not_run", lint: "not_run" });
  });
});

describe("diffStats", () => {
  const diff = [
    "diff --git a/src/a.ts b/src/a.ts",
    "index 1..2 100644",
    "--- a/src/a.ts",
    "+++ b/src/a.ts",
    "@@ -1,3 +1,4 @@",
    " keep",
    "-old",
    "+new",
    "+more",
    "--- a removed line starting with dashes",
    "diff --git a/README.md b/README.md",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/README.md",
    "@@ -0,0 +1,2 @@",
    "+# Title",
    "+",
    "diff --git a/logo.png b/logo.png",
    "Binary files a/logo.png and b/logo.png differ",
  ].join("\n");
  it("counts per file and totals", () => {
    const s = diffStats(diff);
    expect(s.files).toEqual([
      { path: "src/a.ts", add: 2, del: 2 },
      { path: "README.md", add: 2, del: 0 },
      { path: "logo.png", add: 0, del: 0, binary: true },
    ]);
    expect([s.add, s.del]).toEqual([4, 2]);
  });
  it("handles plain diffs and empty input", () => {
    expect(diffStats("--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-a\n+b\n").files).toEqual([{ path: "x.txt", add: 1, del: 1 }]);
    expect(diffStats("")).toEqual({ files: [], add: 0, del: 0 });
  });
});

describe("commitMessage", () => {
  it("uses the session title, a conventional type and lists files", () => {
    expect(commitMessage({ title: "Add dark mode toggle", prompt: "add a dark mode toggle", files: ["src/a.ts", "src/b.css"] })).toBe("feat: add dark mode toggle\n\n- src/a.ts\n- src/b.css");
  });
  it("falls back to the prompt and infers the type", () => {
    expect(commitMessage({ title: null, prompt: "Please fix the crash on startup.", files: ["main.rs"] }).split("\n")[0]).toBe("fix: fix the crash on startup");
    expect(commitMessage({ title: "", prompt: "", files: ["docs/guide.md"] }).split("\n")[0]).toBe("docs: update guide.md");
    expect(commitMessage({ title: "", prompt: "", files: ["a.ts", "b.ts"] }).split("\n")[0]).toBe("feat: update 2 files");
  });
  it("strips an existing type prefix and truncates long subjects", () => {
    expect(commitMessage({ title: "refactor: split the parser", files: [] })).toBe("refactor: split the parser");
    const long = commitMessage({ title: "x ".repeat(60), files: [] }).split("\n")[0];
    expect(long.length).toBeLessThanOrEqual(64 + 6);
  });
  it("caps the file list", () => {
    const files = Array.from({ length: 13 }, (_, i) => `f${i}.ts`);
    expect(commitMessage({ title: "t", files })).toContain("- …and 3 more");
  });
  it("commitType falls back on file kinds", () => {
    expect(commitType("", ["src/a.test.ts", "tests/b.py"])).toBe("test");
    expect(commitType("speed up the loader", ["a.ts"])).toBe("perf");
  });
});

describe("over the mock engine", () => {
  const until = async (f: () => boolean, ms = 3000) => {
    const t0 = Date.now();
    while (!f()) {
      if (Date.now() - t0 > ms) throw new Error("timed out");
      await new Promise((r) => setTimeout(r, 5));
    }
  };

  it("an edit + test turn yields checks, git changes, and a commit clears them", async () => {
    const { createMockPlatform } = await import("../src/platform/mock");
    const { App } = await import("../src/state/app");
    const { lastTurn } = await import("../src/ui/code/commitReady");
    const app = new App(createMockPlatform({ tickMs: 0 }));
    await app.start();
    await until(() => app.models.length > 0);
    const id = await app.createSession({ kind: "code" });
    const s = app.session(id);
    await until(() => !s.loading);
    await s.send([{ type: "text", text: "edit a file and run tests" }]);
    await until(() => !s.busy && s.model.messages.some((m) => m.role === "assistant" && m.parts.some((p) => p.kind === "text" && p.final)));
    const turn = lastTurn(s.model);
    expect(turn.prompt).toBe("edit a file and run tests");
    expect(turn.calls.map((c) => c.name)).toEqual(["edit", "write", "shell"]);
    expect(turnChecks(turn.calls)).toMatchObject({ changedFiles: true, tests: "ok", lint: "not_run" });
    const status = await app.client.git.status({ session_id: id });
    expect(status.files.map((f) => f.path)).toEqual(["src/App.tsx", "src/App.test.tsx"]);
    const stats = diffStats((await app.client.git.diff({ session_id: id })).text);
    expect(stats).toMatchObject({ add: 5, del: 1 });
    await app.client.git.commit({ session_id: id, message: "feat: x" });
    expect((await app.client.git.status({ session_id: id })).files).toEqual([]);
  });
});
