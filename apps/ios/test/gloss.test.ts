import { describe, expect, it } from "vitest";

import { gloss } from "../src/gloss";

/**
 * Case for case with `the_gloss_reads_the_expressions_people_write` and
 * `an_expression_the_gloss_cannot_read_comes_back_untouched` in
 * `cli/src/tui/data.rs`.
 *
 * Every string asserted here is asserted identically on the Rust side, so a
 * schedule cannot read one way in the terminal and another on the phone.
 */
describe("the cron gloss, against cli/src/tui/data.rs", () => {
  it("reads the expressions people actually write", () => {
    expect(gloss("0 2 * * *")).toBe("02:00 every day");
    expect(gloss("*/15 * * * *")).toBe("every 15 minutes");
    expect(gloss("0 9 * * 1-5")).toBe("09:00 Mon–Fri");
    expect(gloss("0 * * * *")).toBe("every hour at :00");
    expect(gloss("30 * * * *")).toBe("every hour at :30");
    expect(gloss("0 */2 * * *")).toBe("every 2 hours");
    expect(gloss("* * * * *")).toBe("every minute");
    expect(gloss("0 0 * * 0")).toBe("00:00 Sun");
    expect(gloss("15 6 * * 1,3,5")).toBe("06:15 Mon, Wed, Fri");
    expect(gloss("*/10 * * * 6,0")).toBe("every 10 minutes, Sat, Sun");
    expect(gloss("0 9 * * MON-FRI")).toBe("09:00 Mon–Fri");
  });

  /**
   * A wrong gloss is worse than none: the expression is what decides when an
   * agent runs unattended, so anything not read with certainty comes back
   * exactly as it was written.
   */
  it("hands back untouched anything it cannot read", () => {
    for (const cron of [
      // A day-of-month restriction, which the gloss says nothing about.
      "0 2 1 * *",
      // A month restriction, likewise.
      "0 2 * 6 *",
      // Six fields: croner's seconds-first form. Reading it as five would shift
      // every field by a place.
      "0 0 2 * * *",
      // Lists and ranges in the minute and hour fields.
      "0,30 9 * * *",
      "0 9-17 * * *",
      // Not an expression at all.
      "@daily",
      "",
    ]) {
      expect(gloss(cron), `${cron} must not be guessed at`).toBe(cron);
    }
  });

  /**
   * The dash is an en dash, not a hyphen. It is the kind of difference that
   * survives review and then shows up as two spellings of one range.
   */
  it("joins a weekday range with an en dash", () => {
    expect(gloss("0 9 * * 1-5")).toContain("–");
    expect(gloss("0 9 * * 1-5")).not.toContain("Mon-Fri");
  });

  // A step of 1 is not worth spelling out; "every 1 minutes" is worse English
  // than the expression. (Line comment: the step syntax closes a block comment.)
  it("does not spell out a step of one", () => {
    expect(gloss("*/1 * * * *")).toBe("*/1 * * * *");
  });

  /** Sunday is both 0 and 7, which is what every cron accepts. */
  it("accepts either spelling of Sunday", () => {
    expect(gloss("0 0 * * 7")).toBe("00:00 Sun");
    expect(gloss("0 0 * * 0")).toBe("00:00 Sun");
  });

  /** `?` is the other way of writing "any day". */
  it("reads a question mark as every day", () => {
    expect(gloss("0 2 * * ?")).toBe("02:00 every day");
  });

  it("names a weekday case-insensitively", () => {
    expect(gloss("0 9 * * mon-fri")).toBe("09:00 Mon–Fri");
  });

  it("refuses a weekday it cannot name", () => {
    expect(gloss("0 9 * * 8")).toBe("0 9 * * 8");
    expect(gloss("0 9 * * XYZ")).toBe("0 9 * * XYZ");
  });

  /**
   * Guards the port rather than the original: JavaScript's `Number` accepts
   * things Rust's `parse::<u32>()` rejects, and each would produce a confident
   * wrong answer.
   */
  it("refuses a field that only looks like a number", () => {
    for (const cron of ["1.5 2 * * *", "0 2.0 * * *", "+1 2 * * *", "-1 2 * * *"]) {
      expect(gloss(cron), `${cron} must not be guessed at`).toBe(cron);
    }
  });

  /** Extra whitespace is not a different expression. */
  it("is not confused by extra spacing", () => {
    expect(gloss("0   2 * * *")).toBe("02:00 every day");
  });
});
