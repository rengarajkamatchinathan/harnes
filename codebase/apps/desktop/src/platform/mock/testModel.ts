// "Genie Test" (mock/genie-test): a scripted model for exercising the chat UI without a real
// provider. It picks a reply from keywords in the prompt; type "help" to list them.

export interface TestReply {
  /** Streamed first as a reasoning part, when set. */
  thinking?: string;
  text: string;
  /** Ends the turn as failed with this message after streaming `text`. */
  error?: string;
  /** Runs a shell tool first with this (large) output: fills the context to demo the context meter. */
  log?: { command: string; output: string };
}

/** ~43k characters (~11k tokens) of verbose test output: two runs pass 70% of a 32k window. */
export function verboseLog(): string {
  const lines: string[] = [];
  for (let i = 1; i <= 450; i++) lines.push(`  ✓ src/feature${i % 37}.test.ts > case ${i}: renders, updates and cleans up without leaking (${(i % 9) + 1} ms)`);
  return `$ vitest run --reporter=verbose\n${lines.join("\n")}\n\n Test Files  37 passed (37)\n      Tests  450 passed (450)`;
}

const HELP = `I'm **Genie Test**, a scripted model for trying out the chat. Include one of these words in your message:

| Keyword | What you get |
|---|---|
| \`help\` | this list |
| \`markdown\` | headings, lists, quotes, links, inline formatting |
| \`code\` | code blocks in several languages |
| \`table\` | a wide table |
| \`math\` | inline and block KaTeX |
| \`mermaid\` | a diagram |
| \`long\` | a long reply, to test scrolling and streaming |
| \`think\` | a reasoning block before the answer |
| \`logs\` | a large tool output that fills the context (small 32k window) |
| \`run\` / \`shell\` / \`permission\` | a tool call that asks for permission |
| \`error\` | a turn that fails partway |

Anything else is echoed back with some stats.`;

const MARKDOWN = `# Heading 1
## Heading 2
### Heading 3

Plain text with **bold**, *italic*, ~~strikethrough~~, \`inline code\` and a [link](https://example.com).

- Bullet one
- Bullet two
  - Nested bullet
- Bullet three

1. First
2. Second
3. Third

- [x] Done task
- [ ] Open task

> A blockquote that spans
> two lines.

---

Final paragraph after a rule.`;

const CODE = `TypeScript:

\`\`\`ts
export async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(\`HTTP \${res.status}\`);
  return (await res.json()) as T;
}
\`\`\`

Rust:

\`\`\`rust
fn main() {
    let total: i32 = (1..=10).filter(|n| n % 2 == 0).sum();
    println!("sum of evens = {total}");
}
\`\`\`

Python:

\`\`\`python
def fib(n: int) -> list[int]:
    a, b, out = 0, 1, []
    for _ in range(n):
        out.append(a)
        a, b = b, a + b
    return out
\`\`\`

Shell:

\`\`\`bash
pnpm install && pnpm dev
\`\`\``;

const TABLE = `| Model | Provider | Context | Input $/M | Output $/M | Notes |
|---|---|---:|---:|---:|---|
| Genie Small | mock | 128K | 0.00 | 0.00 | Canned sample reply |
| Genie Large | mock | 200K | 0.00 | 0.00 | Canned sample reply, effort levels |
| Genie Test | mock | 200K | 0.00 | 0.00 | Scripted replies by keyword |
| A very long model name to test wrapping | example | 1M | 1.25 | 10.00 | This cell has a lot of text so you can see how long content wraps |`;

const MATH = `Inline: Euler's identity $e^{i\\pi} + 1 = 0$, and the area of a circle is $\\pi r^2$.

Block:

$$
\\int_{-\\infty}^{\\infty} e^{-x^2}\\,dx = \\sqrt{\\pi}
$$

$$
\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}
$$`;

const MERMAID = `\`\`\`mermaid
flowchart LR
  U[User] --> C[Composer]
  C --> E[Engine]
  E --> M[(Model)]
  M --> E --> V[Chat view]
\`\`\``;

function long(): string {
  const parts: string[] = ["Here is a long reply to test streaming and scrolling.\n"];
  for (let i = 1; i <= 12; i++) {
    parts.push(
      `## Section ${i}\n\nParagraph ${i}. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.\n`,
    );
  }
  parts.push("That's the end of the long reply.");
  return parts.join("\n");
}

function echo(prompt: string): string {
  const words = prompt.trim().split(/\s+/).filter(Boolean).length;
  const quoted = prompt
    .trim()
    .split("\n")
    .map((l) => `> ${l}`)
    .join("\n");
  return `You said:\n\n${quoted}\n\nThat's **${words}** word${words === 1 ? "" : "s"} and **${prompt.length}** character${prompt.length === 1 ? "" : "s"}. Type \`help\` to see what else I can do.`;
}

export function testReply(prompt: string): TestReply {
  const p = prompt.toLowerCase();
  const has = (...words: string[]) => words.some((w) => new RegExp(`\\b${w}\\b`).test(p));

  if (has("error", "fail")) return { text: "Starting the answer, and then something goes wrong…", error: "Genie Test simulated a provider error (you asked for one)." };
  const thinking = has("think", "reason") ? "The user wants to see a reasoning block. I'll think for a moment, then give a short answer that explains what happened." : undefined;
  if (has("help")) return { thinking, text: HELP };
  if (has("markdown", "md")) return { thinking, text: MARKDOWN };
  if (has("code")) return { thinking, text: CODE };
  if (has("table")) return { thinking, text: TABLE };
  if (has("math", "latex", "katex")) return { thinking, text: MATH };
  if (has("mermaid", "diagram")) return { thinking, text: MERMAID };
  if (has("long")) return { thinking, text: long() };
  if (has("logs", "verbose")) return { thinking, text: "All 450 tests passed. That output was large: send `logs` once or twice more to watch the context meter fill up.", log: { command: "npm test -- --reporter=verbose", output: verboseLog() } };
  if (has("run", "shell", "permission")) return { thinking, text: "The command finished. Its output is shown above." };
  if (thinking) return { thinking, text: "Done thinking. The reasoning above is collapsible." };
  return { text: echo(prompt) };
}
