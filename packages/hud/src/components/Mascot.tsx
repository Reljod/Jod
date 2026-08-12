/** Jod's mascot: a small green lion cub under a spiky mane, sitting.
 *
 *  The same character the TUI draws in block characters, in the one medium that
 *  is not stuck on a cell grid. Same palette, same proportions: head and mane
 *  three quarters of the box, a green body far too small for it underneath, and
 *  a mane of two spike lengths — long red between short orange — because a ring
 *  of even points reads as a saw blade and it is the ragged edge that reads as
 *  fur. The body is drawn far too small on purpose: a body to scale reads as a
 *  lion, and one drawn far too small reads as a cub.
 *
 *  Filled and polychrome, where this used to be `currentColor` line art. The
 *  reason the old outline existed was that a filled face would have needed the
 *  panel's own background to read as a hole, and the mark sits on several
 *  backgrounds. Nothing here reads as a hole — every shape is opaque and the
 *  palette is fixed — so the mark is correct on any of them, and it is the same
 *  lion in the topbar as in the terminal rather than a cyan cousin of it.
 *
 *  The palette is the xterm-256 slots the TUI uses, resolved to hex, so the two
 *  drawings are the same colours and not merely the same idea of them.
 */

const SPIKE = "#ff0000"; // 196 — the long spikes
const MANE_C = "#ff8700"; // 208 — the mane between them, and the nose
const FACE = "#ffaf5f"; // 215 — the face it all rings
const EYE = "#ffffff"; // 231
const PUPIL = "#121212"; // 233
const MAW = "#870000"; // 88 — the open throat
const FUR = "#00d75f"; // 41 — the cub
const PAW = "#5cffa8"; // the pads, a shade up from FUR so they read against it
const NOSE = "#e2670a"; // MANE, darkened just enough to carry against FACE
const MOUTH = "#a03a00"; // the muzzle line, between MANE and MAW

/** The two spike rings, ten points each, interleaved so a long red one falls
 *  between every pair of short orange. Generated rather than eyeballed —
 *  hand-written star vertices are how a ring like this ends up lopsided. */
const SPIKES =
  "M12.00 0.10 L14.04 3.32 L17.58 1.91 L17.34 5.72 L21.04 6.66 L18.60 9.60 " +
  "L21.04 12.54 L17.34 13.48 L17.58 17.29 L14.04 15.88 L12.00 19.10 " +
  "L9.96 15.88 L6.42 17.29 L6.66 13.48 L2.96 12.54 L5.40 9.60 L2.96 6.66 " +
  "L6.66 5.72 L6.42 1.91 L9.96 3.32 Z";

const MANE =
  "M14.44 2.09 L15.64 4.58 L18.39 4.96 L17.90 7.68 L19.90 9.60 L17.90 11.52 " +
  "L18.39 14.24 L15.64 14.62 L14.44 17.11 L12.00 15.80 L9.56 17.11 " +
  "L8.36 14.62 L5.61 14.24 L6.10 11.52 L4.10 9.60 L6.10 7.68 L5.61 4.96 " +
  "L8.36 4.58 L9.56 2.09 L12.00 3.40 Z";

interface Props {
  /** Rendered size in pixels. The art is a 24-unit square, so it scales. */
  size?: number;
  /** Extra classes for the call site to position it with. */
  className?: string;
  /** Something is running, so the lion scratches at the itch. The mascot is
   *  the only part of a header that can carry state without adding a widget. */
  busy?: boolean;
}

/** One eye: white, pupil, and the catchlight that is most of what makes it
 *  read as an eye rather than a dot. `side` is what lets the scratching squint
 *  land on the eye the paw is next to. */
function Eye({ x, side }: { x: number; side: "left" | "right" }) {
  return (
    <g className={`mascot-eye mascot-eye-${side}`}>
      <ellipse cx={x} cy="8.9" rx="2.05" ry="2.25" fill={EYE} />
      <circle cx={x + (side === "left" ? 0.25 : -0.25)} cy="9.25" r="1.05" fill={PUPIL} />
      <circle cx={x + (side === "left" ? -0.2 : -0.7)} cy="8.5" r="0.42" fill={EYE} />
    </g>
  );
}

export function Mascot({ size = 20, className = "", busy = false }: Props) {
  return (
    <svg
      className={`mascot ${busy ? "mascot-busy " : ""}${className}`.trim()}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      // A decorative mark beside a wordmark that already says JOD: naming it
      // again would make a screen reader read the brand twice.
      aria-hidden="true"
      focusable="false"
    >
      {/* Tail first, so the body covers the root of it. It sweeps low and out
          rather than arcing back over the body: an arc that rises above the
          shoulder stops reading as a tail and starts reading as a handle. */}
      <path
        d="M14.6 21.8 Q19.3 22.6 20.2 20.2"
        fill="none"
        stroke={FUR}
        strokeWidth="1.35"
        strokeLinecap="round"
      />
      <circle cx="20.5" cy="19.6" r="0.95" fill={FUR} />
      <ellipse className="mascot-body" cx="11.7" cy="20.5" rx="3.9" ry="3.3" fill={FUR} />
      {/* Two forepaws, a shade lighter so they separate from the body they sit
          against. Without them the cub is a ball. */}
      <ellipse cx="9.9" cy="23.0" rx="1.5" ry="1.05" fill={PAW} />
      <ellipse cx="13.4" cy="23.0" rx="1.5" ry="1.05" fill={PAW} />

      {/* The crown is one group so the roar can puff the whole mane at once. */}
      <g className="mascot-crown">
        <path d={SPIKES} fill={SPIKE} />
        <path d={MANE} fill={MANE_C} />
      </g>
      <circle cx="12" cy="9.6" r="5.6" fill={FACE} />

      <g className="mascot-eyes">
        <Eye x={9.5} side="left" />
        <Eye x={14.5} side="right" />
      </g>

      <path
        className="mascot-nose"
        d="M11.05 10.95 Q12 10.65 12.95 10.95 Q12 12.35 11.05 10.95 Z"
        fill={NOSE}
      />

      {/* Mouth shut, and the same mouth open. The roar cross-fades the two
          rather than morphing one, which is the only way to get a throat to
          appear behind a lip that was a stroke a moment ago.

          The lobes are kept short and the fangs hang off their ends. Drawn any
          wider the mouth stops being a muzzle and becomes a moustache. */}
      <g className="mascot-muzzle">
        <path
          d="M12 12.1 L12 12.9 M12 12.9 Q10.85 12.9 10.6 12.05 M12 12.9 Q13.15 12.9 13.4 12.05"
          fill="none"
          stroke={MOUTH}
          strokeWidth="0.8"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        <path d="M10.25 12.8 L11.15 12.8 L10.7 14.05 Z" fill={EYE} />
        <path d="M12.85 12.8 L13.75 12.8 L13.3 14.05 Z" fill={EYE} />
      </g>
      <g className="mascot-maw">
        <ellipse cx="12" cy="13.2" rx="2.7" ry="2.0" fill={MAW} />
        <path d="M10.1 11.6 L11.1 11.6 L10.6 13.3 Z" fill={EYE} />
        <path d="M12.9 11.6 L13.9 11.6 L13.4 13.3 Z" fill={EYE} />
      </g>

      {/* Up at the cheek, in front of the mane, and hidden until work runs.
          Three toes on a pad rather than a plain circle — one blob beside a
          head is a leaf, and it takes the toes to make it a paw. */}
      <g className="mascot-paw">
        <ellipse cx="18.4" cy="11.5" rx="2.1" ry="1.8" fill={FUR} />
        <circle cx="16.8" cy="9.9" r="0.68" fill={FUR} />
        <circle cx="18.5" cy="9.35" r="0.68" fill={FUR} />
        <circle cx="20.1" cy="10.1" r="0.68" fill={FUR} />
        <ellipse cx="18.4" cy="11.75" rx="1.05" ry="0.9" fill={PAW} />
      </g>
    </svg>
  );
}
