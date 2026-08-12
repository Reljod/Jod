/**
 * A cron expression in English.
 *
 * A direct port of `gloss` in [`cli/src/tui/data.rs`](../../../cli/src/tui/data.rs),
 * because the API deliberately sends the raw `cron` and `timezone` rather than a
 * rendered sentence — a gloss is a rendering choice and a relative timestamp is
 * true for the second it was made, so neither belongs on the wire or in a cache.
 *
 * Ported rather than reinvented so a schedule reads the same on the phone as in
 * the terminal. `test/gloss.test.ts` runs the Rust suite's cases verbatim.
 *
 * **A wrong gloss is worse than none.** The expression is what decides when an
 * agent runs unattended, so anything this cannot read with certainty comes back
 * exactly as it was written.
 */
export function gloss(cron: string): string {
  return describe(cron) ?? cron;
}

/** The gloss, or `null` for anything not read with certainty. */
function describe(cron: string): string | null {
  const fields = cron.split(/\s+/).filter((f) => f !== "");
  // Five fields, and only five. croner also accepts a six-field form with
  // seconds in front, and reading that one as this one shifts every field by a
  // place — `0 0 2 * * *` would gloss as `00:02 every day`.
  if (fields.length !== 5) return null;
  const [minute, hour, dayOfMonth, month, dayOfWeek] = fields;

  // A day-of-month or a month restriction changes which days fire, and this says
  // nothing about days beyond the weekday field. Rather than gloss half of it,
  // hand the whole expression back.
  if (dayOfMonth !== "*" || month !== "*") return null;

  // A clock time reads as `09:00 Mon–Fri`; an interval reads as `every 15
  // minutes, Mon–Fri`. Same two parts, different joins.
  let when: string;
  let isClock = false;

  if (minute === "*" && hour === "*") {
    when = "every minute";
  } else if (hour === "*") {
    const every = step(minute);
    if (every !== null) {
      when = `every ${every} minutes`;
    } else {
      const at = number(minute);
      if (at === null) return null;
      when = `every hour at :${pad(at)}`;
    }
  } else if (minute === "0" && step(hour) !== null) {
    when = `every ${step(hour)} hours`;
  } else {
    const h = number(hour);
    const m = number(minute);
    if (h === null || m === null) return null;
    when = `${pad(h)}:${pad(m)}`;
    isClock = true;
  }

  const days = weekdays(dayOfWeek);
  if (days === null) return null;

  if (isClock) return days === "every day" ? `${when} every day` : `${when} ${days}`;
  return days === "every day" ? when : `${when}, ${days}`;
}

// The `N` of a `*/N` step, and nothing else. A step of 1 is not worth spelling
// out — "every 1 minutes" is worse English than the expression itself.
//
// A line comment rather than a doc comment: the step syntax closes a block
// comment, and escaping it would leave the wrong text in the source.
function step(field: string): number | null {
  if (!field.startsWith("*/")) return null;
  const n = number(field.slice(2));
  return n !== null && n > 1 ? n : null;
}

/** A whole non-negative number, or `null`. Rust's `parse::<u32>()` accepts no
 *  sign, no decimal point and no surrounding text, and neither does this —
 *  `Number("1.5")` and `Number(" 1 ")` would both otherwise slip through. */
function number(field: string): number | null {
  if (!/^\d+$/.test(field)) return null;
  return Number(field);
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** The day-of-week field as names, or `null` for anything it cannot name exactly. */
function weekdays(field: string): string | null {
  if (field === "*" || field === "?") return "every day";

  const dash = field.indexOf("-");
  if (dash !== -1) {
    const from = weekday(field.slice(0, dash));
    const to = weekday(field.slice(dash + 1));
    // An en dash, as the TUI writes it.
    return from !== null && to !== null ? `${from}–${to}` : null;
  }

  const named = field.split(",").map(weekday);
  return named.every((d) => d !== null) ? named.join(", ") : null;
}

/**
 * One day, by number or by name. Both 0 and 7 are Sunday, which is what every
 * cron implementation accepts and what people write.
 */
function weekday(field: string): string | null {
  switch (field.trim().toUpperCase()) {
    case "0":
    case "7":
    case "SUN":
      return "Sun";
    case "1":
    case "MON":
      return "Mon";
    case "2":
    case "TUE":
      return "Tue";
    case "3":
    case "WED":
      return "Wed";
    case "4":
    case "THU":
      return "Thu";
    case "5":
    case "FRI":
      return "Fri";
    case "6":
    case "SAT":
      return "Sat";
    default:
      return null;
  }
}
