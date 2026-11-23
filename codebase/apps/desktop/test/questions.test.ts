import { describe, expect, it } from "vitest";
import { boldChoices, formatAnswers, parseQuestions } from "../src/ui/chat/questions";

const WEEK = `To plan your week well, I need just a little structure from you.

### 5 quick questions (reply in bullets)
1. What's your **timezone** and do you want the plan to start **Mon–Sun** or **Sun–Sat**?
2. What are your **fixed commitments** this week (work/school hours, meetings, appointments) and which **days**?
3. What are your **top 3 outcomes** by end of week (work + personal)?
4. Any **constraints**: deadlines (with dates), energy limits, travel, childcare, "no work after X pm," etc.?
5. What planning style do you want: **time-blocked calendar**, **daily to-do lists**, or a **hybrid**?

### While you answer, here's a solid default weekly template (hybrid)
- **Daily (Mon–Fri)**
  - 10 min: plan the day`;

describe("question stepper parsing", () => {
  it("reads numbered questions and their bold choices", () => {
    const qs = parseQuestions(WEEK)!;
    expect(qs).toHaveLength(5);
    expect(qs[0].text).toBe("What's your timezone and do you want the plan to start Mon–Sun or Sun–Sat?");
    expect(qs[0].options).toEqual(["Mon–Sun", "Sun–Sat"]);
    expect(qs[1].options).toEqual([]);
    expect(qs[4].options).toEqual(["time-blocked calendar", "daily to-do lists", "hybrid"]);
  });

  it("prefers a fenced questions block", () => {
    const md = 'A couple of things first.\n\n```questions\n{"questions":[{"q":"Which stack?","options":["React","Vue"]},{"q":"Anything else?"}]}\n```';
    expect(parseQuestions(md)).toEqual([
      { text: "Which stack?", options: ["React", "Vue"], multi: false },
      { text: "Anything else?", options: [], multi: false },
    ]);
  });

  it("ignores plain lists and single questions", () => {
    expect(parseQuestions("1. Install it\n2. Run it")).toBeNull();
    expect(parseQuestions("1. Is it on?\n2. Run it")).toBeNull();
  });

  it("only treats joined bold runs as choices", () => {
    expect(boldChoices("Use **A** then later **B**?")).toEqual([]);
    expect(boldChoices("**yes** or **no**?")).toEqual(["yes", "no"]);
  });

  it("formats answers, marking skipped ones", () => {
    expect(formatAnswers([{ text: "A?", options: [] }, { text: "B?", options: [] }], ["x", ""])).toBe("Here are my answers:\n\n1. A?\n   → x\n2. B?\n   → (skipped)");
  });
});
