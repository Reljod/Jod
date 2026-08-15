# The TUI itself

How this was tested: the binary built from this branch, running inside tmux on
its own socket, under a throwaway `JOD_HOME`, driven by sending real keystrokes
and capturing the pane. Terminal sizes 200x50, 110x42 and 40x20.

The headline is that the console is in good shape. Ten scenarios were run and
most passed cleanly, including the ones most likely to break. The findings below
are small; none of them is the bug Reljod reported. That bug is in
[`00-launch-and-roots.md`](00-launch-and-roots.md) and
[`01-routing.md`](01-routing.md).

---

## T1. Text is lost when a double-width character sits near the wrap column
Status: **fixed — merged as #158** · Severity was: low

> **My hypothesis in this task was wrong, and the fix's measurement says how.**
> I wrote that it "looks like a miscount only when the wide character's cells
> straddle the boundary", making it an off-by-one rather than a missing
> wide-character case — and I reasoned that from the emoji twin passing.
>
> Measured: **every row containing a wide character overflows**, not only a
> straddling one. The twin that "passed" passed because the single clipped
> column happened to hold a space, so nothing visible was lost. I had taken an
> invisible failure as a passing case and built a theory on the difference
> between them.
>
> The lesson is narrower than "reasoning is unreliable" and worth stating
> exactly: **a passing case that differs from a failing one by a space is not a
> contrast, it is the same failure with nothing in the gap.** Where a test's
> pass depends on which character landed in a clipped column, the character has
> to be part of what is asserted.

At 40 columns, a line containing Japanese characters lost three characters at
the wrap:

```
typed:  AAAA BBBB CCCC 日本語 DDDD EEEE FFFF
shown:  › AAAA BBBB CCCC 日本語 DDDD EEEE
          F
```

`FFFF` became `F`. An earlier run lost five characters the same way, from
`tanong: anong ginagawa? 日本語 🚀 café` — the rocket and the `ca` of `café`
both disappeared, at the same terminal width.

What stops this being a confirmed finding is that the obvious twin passed. The
same length of text with one emoji instead of CJK wrapped correctly and lost
nothing:

```
typed:  AAAA BBBB CCCC 🚀 DDDD EEEE FFFF GGGG
shown:  › AAAA BBBB CCCC 🚀 DDDD EEEE FFFF
          GGGG
```

So it is not simply "wide characters break". It looks like a miscount only when
the wide character's cells straddle the boundary, which would make it an
off-by-one in the width accounting rather than a missing wide-character case.
Plain ASCII wraps correctly at the same width, and tmux renders the identical
string correctly on its own, so the terminal is not the cause.

Before fixing: print what the composer thinks the string's display width is at
the moment it wraps. `cli/src/tui/text.rs` is where the wrapping lives.

Check: a property test over the composer's wrapper asserting that the wrapped
lines rejoin to exactly the input, for a mix of ASCII, CJK and emoji at every
width from 20 to 120.

---

## Scenarios run

| # | Scenario | Expected | Actual | |
|---|---|---|---|---|
| 1 | Opening screen, 110x42 | banner, directory, composer, status bar | all four, and the directory is named | pass |
| 2 | Opening screen, 200x50 | same, centred | same | pass |
| 3 | `/` opens the command menu | a scrollable list of commands | 40-odd commands, each with one line of help | pass |
| 4 | `/root` | lists the console's roots | listed `tui-repo`, marked `ro` for read-only | pass |
| 5 | Ctrl-F, empty fleet | says nothing is running | status bar reads "nothing delegated yet" | pass |
| 6 | Ctrl-G | the menu of every screen | full menu, with "Esc cancels · any other key is ignored" | pass |
| 7 | Escape from a screen | back to the chat | back to the chat | pass |
| 8 | A 160-word instruction in the composer | wraps, does not truncate | wrapped across six lines, all present | pass |
| 9 | Unicode and emoji at 110 columns | renders intact | `tanong: anong ginagawa? 日本語 🚀 café` intact | pass |
| 10 | The same at 40 columns | renders intact | lost `🚀 ca` | **fail — T1** |
| 11 | Resize 110 → 40 → 110 | reflows, keeps the text | reflowed; text restored intact at 110 | pass |
| 12 | ASCII wrapping at 40 columns | wraps losslessly | losslessly | pass |
| 13 | Emoji near the wrap column | wraps losslessly | losslessly | pass |
| 14 | CJK near the wrap column | wraps losslessly | lost three characters | **fail — T1** |
| 15 | A pane of nothing but emoji | wraps losslessly | 16 of 20 shown, rest wrapped out of view | inconclusive |

Two notes on things that looked like bugs and were not:

- `/root` appears to print nothing at 110 columns. It does print; the output is
  above the visible region of the captured pane. The narrow-terminal run is
  what showed it, which is a reminder that an empty-looking capture is not an
  empty transcript.
- The fleet's two panes are empty boxes when nothing is running, with the empty
  state only in the status bar. That is a defensible choice rather than a bug,
  but a line inside the pane would read better than a blank box.
